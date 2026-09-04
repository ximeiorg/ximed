//! 已安装剪贴板同步插件管理：列表 / 配置 / 安装 / 构建传输通道。
//!
//! 包装 libximecore `xime_plugin::PluginManager`，目录布局（同 XimeChe）：
//! - `<root>/<id>/` 解压后的插件包
//! - `<root>/registry.yaml` 安装记录
//! - `<root>/config/<id>.yaml` 插件配置——宿主设置表单与插件运行时 `host.config` 同源
//!
//! 配置文件为扁平 String→String YAML，与 `xime-plugin` runtime 的读写格式一致。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use xime_plugin::manager::ManagerError;
use xime_plugin::{
    PluginManager, PluginManifest, PluginRecord, PluginRecordState, PluginRuntime, PluginType,
};

use super::schema::{PluginConfigField, parse_config_schema};
use crate::transport::{Transport, TransportError};

/// 同步插件管理错误。
#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("io 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("插件管理: {0}")]
    Manager(#[from] ManagerError),
    #[error("插件运行时: {0}")]
    Runtime(#[from] xime_plugin::RuntimeError),
    #[error("插件未安装: {0}")]
    NotFound(String),
}

/// 剪贴板同步插件管理器（只消费 clipboard_sync 类型插件）。
pub struct SyncPluginManager {
    inner: PluginManager,
}

impl SyncPluginManager {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            inner: PluginManager::new(root),
        }
    }

    /// 插件根目录（设置页提示安装路径用）。
    pub fn root(&self) -> &Path {
        self.inner.root()
    }

    /// 已安装且目录就绪的 clipboard_sync 插件。
    pub fn list_sync(&self) -> Vec<PluginRecord> {
        self.inner
            .list()
            .into_iter()
            .filter(|p| {
                p.plugin_type.eq_ignore_ascii_case("clipboard_sync")
                    && p.state == PluginRecordState::Ready
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<PluginRecord> {
        self.inner.get(id)
    }

    /// 已安装插件的 manifest。
    pub fn manifest(&self, id: &str) -> Result<PluginManifest, RegistryError> {
        Ok(self.inner.load_manifest(id)?)
    }

    /// manifest 声明的配置字段（configSchema）；manifest 缺失/损坏时为空。
    pub fn config_fields(&self, id: &str) -> Vec<PluginConfigField> {
        self.manifest(id)
            .map(|m| parse_config_schema(&m.config_schema))
            .unwrap_or_default()
    }

    /// 读取插件配置（`host.config` 同源文件）；文件缺失或损坏视为空配置。
    pub fn load_config(&self, id: &str) -> Result<BTreeMap<String, String>, RegistryError> {
        let path = self.inner.config_path(id);
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&content).unwrap_or_default())
    }

    /// 写入插件配置；通道下次重建（保存设置/重启引擎）即生效。
    pub fn save_config(
        &self,
        id: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<(), RegistryError> {
        let path = self.inner.config_path(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let yaml =
            serde_yaml::to_string(values).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, yaml)?;
        Ok(())
    }

    /// 安装 .xipk（转发 `PluginManager`；同版本需 force 覆盖）。
    pub fn install_from_zip(&self, xipk: &Path, force: bool) -> Result<PluginRecord, ManagerError> {
        self.inner.install_from_zip(xipk, force)
    }

    /// 卸载插件（目录、配置、registry 一并清理）。
    pub fn uninstall(&self, id: &str) -> Result<(), ManagerError> {
        self.inner.uninstall(id)
    }

    /// 构建插件传输通道（失败多为 manifest 缺失/类型不符/入口加载失败）。
    pub fn make_transport(&self, id: &str) -> Result<Arc<dyn Transport>, TransportError> {
        let manifest = self
            .manifest(id)
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        if manifest.plugin_type() != PluginType::ClipboardSync {
            return Err(TransportError::Connect(format!(
                "plugin {id} is not clipboard_sync"
            )));
        }
        super::make_plugin_transport(&self.inner.plugin_dir(id), &self.inner.config_path(id))
    }

    /// 调用插件 `testConnection` 契约：`Ok(None)` 成功，`Ok(Some(消息))` 为失败原因。
    /// 使用已落盘的插件配置（表单测试前应先保存）。
    /// 插件未实现该契约函数时返回 `Ok(Some("插件未实现…"))`，不发请求也不误报成功。
    pub fn test_connection(&self, id: &str) -> Result<Option<String>, RegistryError> {
        let manifest = self.manifest(id)?;
        let runtime = PluginRuntime::load(
            &self.inner.plugin_dir(id),
            &manifest.entry,
            &self.inner.config_path(id),
        )?;
        if !runtime.has_fn("testConnection") {
            return Ok(Some("插件未实现 testConnection，无法测试连接".into()));
        }
        Ok(runtime.test_connection())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::schema::PluginFieldType;
    use std::io::Write;

    /// 打包 clipboard_sync fixture 插件为 .xipk（manifest 带 configSchema）。
    fn fixture_xipk(dir: &Path, test_connection_returns: &str) -> PathBuf {
        let xipk = dir.join("fixture.xipk");
        let file = std::fs::File::create(&xipk).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.yaml", options).unwrap();
        zip.write_all(
            "id: com.example.sync_fixture\n\
             name: Fixture Sync\n\
             version: 1.0.0\n\
             type: clipboard_sync\n\
             activation: single\n\
             entry: main.lua\n\
             configSchema:\n\
             \x20 - key: serverUrl\n\
             \x20   label: 服务器地址\n\
             \x20   type: text\n\
             \x20   placeholder: https://host:port\n\
             \x20 - key: password\n\
             \x20   label: 密码\n\
             \x20   type: secret\n\
             \x20   required: false\n\
             \x20 - key: testConnection\n\
             \x20   label: 测试连接\n\
             \x20   type: button\n\
             \x20   action: testConnection\n"
                .as_bytes(),
        )
        .unwrap();
        zip.start_file("main.lua", options).unwrap();
        zip.write_all(
            format!(
                r#"
local plugin = {{}}
function plugin.push(profile)
    host.config.set("remote", host.json.encode(profile))
    return true
end
function plugin.pull()
    local raw = host.config.get("remote")
    if raw == nil then return nil end
    return host.json.decode(raw)
end
function plugin.testConnection()
    local url = host.config.get("serverUrl")
    if url == nil or url == "" then return "serverUrl 未配置" end
    return {test_connection_returns}
end
return plugin
"#
            )
            .as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
        xipk
    }

    fn test_manager(label: &str) -> (SyncPluginManager, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ximed_plugin_reg_{}_{}", std::process::id(), label));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (SyncPluginManager::new(dir.join("plugins")), dir)
    }

    #[test]
    fn install_list_config_roundtrip() {
        let (manager, dir) = test_manager("roundtrip");
        let xipk = fixture_xipk(&dir, "nil");
        let record = manager.install_from_zip(&xipk, false).unwrap();
        assert_eq!(record.id, "com.example.sync_fixture");

        let list = manager.list_sync();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "Fixture Sync");

        // configSchema → 表单字段（含 button 类型）
        let fields = manager.config_fields("com.example.sync_fixture");
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].key, "serverUrl");
        assert!(fields[0].required);
        assert_eq!(fields[2].field_type, PluginFieldType::Button);

        // 配置读写（与 host.config 同源，扁平 String→String YAML）
        let mut values = manager.load_config("com.example.sync_fixture").unwrap();
        assert!(values.is_empty());
        values.insert("serverUrl".into(), "https://dav.example.com".into());
        values.insert("password".into(), "secret 密".into());
        manager
            .save_config("com.example.sync_fixture", &values)
            .unwrap();
        let loaded = manager.load_config("com.example.sync_fixture").unwrap();
        assert_eq!(
            loaded.get("serverUrl").map(String::as_str),
            Some("https://dav.example.com")
        );
        assert_eq!(
            loaded.get("password").map(String::as_str),
            Some("secret 密")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn transport_uses_saved_config() {
        let (manager, dir) = test_manager("transport");
        let xipk = fixture_xipk(&dir, "nil");
        manager.install_from_zip(&xipk, false).unwrap();
        let id = "com.example.sync_fixture";

        let mut values = manager.load_config(id).unwrap();
        values.insert("serverUrl".into(), "https://x".into());
        manager.save_config(id, &values).unwrap();

        let transport = manager.make_transport(id).unwrap();
        let profile = xime_sync_domain::profile::Profile::from_text("hi", None);
        transport.push(&profile).await.unwrap();
        let got = transport.pull().await.unwrap().expect("profile");
        assert_eq!(got.text, "hi");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_connection_reports_missing_config() {
        let (manager, dir) = test_manager("testconn");
        let xipk = fixture_xipk(&dir, "nil");
        manager.install_from_zip(&xipk, false).unwrap();
        let id = "com.example.sync_fixture";

        // 未配置 serverUrl → 插件返回失败消息
        let err = manager.test_connection(id).unwrap();
        assert_eq!(err.as_deref(), Some("serverUrl 未配置"));

        // 配置后 → nil → 成功
        let mut values = manager.load_config(id).unwrap();
        values.insert("serverUrl".into(), "https://x".into());
        manager.save_config(id, &values).unwrap();
        assert_eq!(manager.test_connection(id).unwrap(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn uninstall_removes_plugin_and_config() {
        let (manager, dir) = test_manager("uninstall");
        let xipk = fixture_xipk(&dir, "nil");
        let id = "com.example.sync_fixture";
        manager.install_from_zip(&xipk, false).unwrap();
        let mut values = manager.load_config(id).unwrap();
        values.insert("serverUrl".into(), "https://x".into());
        manager.save_config(id, &values).unwrap();

        manager.uninstall(id).unwrap();
        assert!(manager.list_sync().is_empty());
        assert!(
            !manager
                .root()
                .join("config")
                .join(format!("{id}.yaml"))
                .exists()
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
