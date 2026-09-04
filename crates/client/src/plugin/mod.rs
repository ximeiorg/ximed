//! Lua 剪贴板同步插件框架。
//!
//! 核心复用 libximecore `xime-plugin`（.xipk 安装、Lua 沙箱运行时），本模块只做三件事：
//! - Rust ↔ Lua 桥接：clipboard_sync 契约（`push`/`pull`/`testConnection`）→ [`Transport`]
//! - [`schema`]：manifest `configSchema` 解释，设置表单数据驱动渲染（无需逐插件硬编码）
//! - [`registry`] / [`index`]：已安装插件管理与商店索引（拉取/下载/校验）
//!
//! 与 Android `LuaClipboardSyncPluginAdapter` 同契约，同一 .xipk 可双端复用。
//! 引擎的去重/回声抑制/退避逻辑完全复用（[`crate::sync::SyncEngine`]）；通道为拉取式，
//! `next_event` 睡眠一个轮询周期后 `pull`（同 HTTP 直连语义）。

mod index;
mod registry;
mod schema;

pub use index::{
    DEFAULT_PLUGIN_INDEX_URL, IndexDownload, IndexError, IndexVersion, PluginIndex,
    PluginIndexEntry, download_xipk, fetch_index, parse_index,
};
pub use registry::{RegistryError, SyncPluginManager};
pub use schema::{PluginConfigField, PluginFieldType, parse_config_schema};

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use xime_plugin::{PluginManifest, PluginRuntime, PluginType};
use xime_sync_domain::profile::Profile;

use crate::transport::{Transport, TransportError};

/// 从已解压的插件目录构建插件传输通道；manifest 非 clipboard_sync 类型时返回错误。
///
/// `config_file` 为插件配置文件路径（[`SyncPluginManager`] 约定为 `<root>/config/<id>.yaml`，
/// 与插件运行时 `host.config` 同源）；须由调用方传入而非放在插件目录内，避免重装被清掉。
pub fn make_plugin_transport(
    plugin_dir: &Path,
    config_file: &Path,
) -> Result<Arc<dyn Transport>, TransportError> {
    let manifest = PluginManifest::from_dir(plugin_dir)
        .map_err(|e| TransportError::Connect(format!("plugin manifest: {e}")))?;
    if manifest.plugin_type() != PluginType::ClipboardSync {
        return Err(TransportError::Connect(format!(
            "plugin {} type {:?} is not clipboard_sync",
            manifest.id,
            manifest.plugin_type()
        )));
    }
    let runtime = Arc::new(
        PluginRuntime::load(plugin_dir, &manifest.entry, config_file)
            .map_err(|e| TransportError::Connect(format!("plugin load: {e}")))?,
    );
    Ok(Arc::new(PluginTransport { runtime }))
}

/// Lua clipboard_sync 插件传输：`push`/`pull`/`testConnection` 桥到 [`Transport`]。
///
/// `host.http` 为同步阻塞调用，全部经 `spawn_blocking` 执行避免阻塞 tokio 执行器；
/// 插件凭据/配置由插件自身经 `host.config` 持久化，本模块不感知。
pub struct PluginTransport {
    runtime: Arc<PluginRuntime>,
}

#[async_trait]
impl Transport for PluginTransport {
    async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
        let json =
            serde_json::to_value(profile).map_err(|e| TransportError::Send(e.to_string()))?;
        let rt = self.runtime.clone();
        let ok = tokio::task::spawn_blocking(move || rt.clipboard_push(&json))
            .await
            .map_err(|e| TransportError::Send(e.to_string()))?;
        if ok {
            Ok(())
        } else {
            Err(TransportError::Send("plugin push returned false".into()))
        }
    }

    async fn pull(&self) -> Result<Option<Profile>, TransportError> {
        let rt = self.runtime.clone();
        let json = tokio::task::spawn_blocking(move || rt.clipboard_pull())
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        match json {
            Some(v) => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| TransportError::Recv(format!("plugin pull parse: {e}"))),
            None => Ok(None),
        }
    }

    async fn next_event(&self, timeout: Duration) -> Result<Option<Profile>, TransportError> {
        // 插件通道为拉取式：睡眠一个轮询周期后拉取（引擎按 hash 去重）
        tokio::time::sleep(timeout).await;
        self.pull().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xime_sync_domain::profile::Profile;

    /// 用 host.config 充当远端存储的 fixture 插件：验证 Rust ↔ Lua profile 桥接。
    fn fixture_plugin_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ximed_plugin_transport_{}_{}",
            std::process::id(),
            label
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.yaml"),
            "id: com.example.clipboard_sync\n\
             name: Test Sync\n\
             version: 1.0.0\n\
             type: clipboard_sync\n\
             activation: single\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.lua"),
            r#"
local plugin = {}
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
    return nil
end
return plugin
"#,
        )
        .unwrap();
        dir
    }

    /// fixture 插件的标准配置文件路径（目录内，仅测试用）。
    fn fixture_config(dir: &Path) -> std::path::PathBuf {
        dir.join("host_config.yaml")
    }

    #[tokio::test]
    async fn push_pull_roundtrip_preserves_profile() {
        let dir = fixture_plugin_dir("roundtrip");
        let t = make_plugin_transport(&dir, &fixture_config(&dir)).unwrap();
        let p = Profile::from_text("hello 你好", Some("dev-a".to_string()));
        t.push(&p).await.unwrap();
        let got = t.pull().await.unwrap().expect("pull must return profile");
        assert_eq!(got.hash, p.hash);
        assert_eq!(got.text, p.text);
        assert_eq!(got.size, p.size);
        assert_eq!(got.source.as_deref(), Some("dev-a"));
        assert_eq!(got.r#type, p.r#type);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn empty_remote_pull_returns_none() {
        let dir = fixture_plugin_dir("empty");
        let t = make_plugin_transport(&dir, &fixture_config(&dir)).unwrap();
        assert!(t.pull().await.unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn next_event_pulls_after_sleep() {
        let dir = fixture_plugin_dir("event");
        let t = make_plugin_transport(&dir, &fixture_config(&dir)).unwrap();
        t.push(&Profile::from_text("x", None)).await.unwrap();
        let got = t
            .next_event(std::time::Duration::from_millis(10))
            .await
            .unwrap();
        assert!(got.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn non_clipboard_sync_plugin_rejected() {
        let dir = fixture_plugin_dir("wrongtype");
        std::fs::write(
            dir.join("manifest.yaml"),
            "id: com.example.emoji\nname: Emoji\nversion: 1.0.0\ntype: emoji\n",
        )
        .unwrap();
        let err = make_plugin_transport(&dir, &fixture_config(&dir))
            .err()
            .expect("non-clipboard_sync plugin must fail");
        assert!(matches!(err, TransportError::Connect(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_manifest_rejected() {
        let dir = std::env::temp_dir().join(format!(
            "ximed_plugin_transport_{}_missing",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let err = make_plugin_transport(&dir, &fixture_config(&dir))
            .err()
            .expect("missing manifest must fail");
        assert!(matches!(err, TransportError::Connect(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
