//! 应用级设置（desktop 形态）：client 连接本机 server 或远程 server。
//!
//! 持久化到 `{config_dir}/ximed-client.toml`，与 server 的 `ximed.toml` 分离，避免互相干扰。

use serde::{Deserialize, Serialize};

use client::config::AdapterKind;
use client::settings::SyncSettings;

/// client 连接模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    /// 连接本机 server（Form 2 默认，地址/端口自动跟随 server 配置）。
    #[default]
    Local,
    /// 连接远程 server。
    Remote,
}

/// 应用级设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// 同步适配器：server（默认）/ plugin。
    /// 非 server 时走插件通道（绕过本机 ximed server），mode/remote_* 字段被忽略。
    #[serde(default)]
    pub adapter: AdapterKind,
    #[serde(default)]
    pub mode: SyncMode,
    /// 远程服务器基础地址，如 `https://example.com:8443`。
    #[serde(default)]
    pub remote_url: String,
    /// 远程服务器用户名。
    #[serde(default)]
    pub remote_username: String,
    /// 远程服务器密码（敏感项明文落盘；desktop 单机场景可接受）。
    #[serde(default)]
    pub remote_password: Option<String>,
    /// 同步插件 id（adapter = plugin 时生效）。
    /// 插件配置值不存这里：存 `{config_dir}/plugins/config/<id>.yaml`（host.config 同源）。
    #[serde(default)]
    pub plugin_id: String,
    /// 同步设置（定义在 client crate，app → client 依赖方向）。
    #[serde(default)]
    pub sync: SyncSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            adapter: AdapterKind::default(),
            mode: SyncMode::Local,
            remote_url: String::new(),
            remote_username: String::new(),
            remote_password: None,
            plugin_id: String::new(),
            sync: SyncSettings::default(),
        }
    }
}

impl AppSettings {
    /// 从 `{config_dir}/ximed-client.toml` 加载；文件缺失或解析失败回退默认值。
    pub fn load(config_dir: &str) -> Self {
        let path = std::path::Path::new(config_dir).join("ximed-client.toml");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 保存到 `{config_dir}/ximed-client.toml`。
    pub fn save(&self, config_dir: &str) -> std::io::Result<()> {
        let dir = std::path::Path::new(config_dir);
        std::fs::create_dir_all(dir)?;
        let text =
            toml::to_string_pretty(self).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(dir.join("ximed-client.toml"), text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_file_returns_default() {
        let dir = tempdir().unwrap();
        assert_eq!(
            AppSettings::load(dir.path().to_str().unwrap()),
            AppSettings::default()
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let s = AppSettings {
            adapter: AdapterKind::Plugin,
            mode: SyncMode::Remote,
            remote_url: "https://example.com:8443".to_string(),
            remote_username: "alice".to_string(),
            remote_password: Some("secret".to_string()),
            plugin_id: "com.example.sync".to_string(),
            sync: SyncSettings {
                switch_on: true,
                push_switch_on: false,
                pull_switch_on: true,
                upload_text: true,
                upload_image: true,
                upload_single_file: true,
                max_file_byte: 5 * 1024 * 1024,
                notify_on_downloaded: false,
            },
        };
        s.save(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(AppSettings::load(dir.path().to_str().unwrap()), s);
    }

    #[test]
    fn corrupted_file_falls_back_to_default() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("ximed-client.toml"), "not toml {{{").unwrap();
        assert_eq!(
            AppSettings::load(dir.path().to_str().unwrap()),
            AppSettings::default()
        );
    }

    #[test]
    fn missing_sync_block_falls_back_to_defaults() {
        // 旧版配置文件没有 sync 节：加载后应回退默认（主开关开）
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("ximed-client.toml"),
            "mode = \"local\"\nremote_url = \"\"\n",
        )
        .unwrap();
        let s = AppSettings::load(dir.path().to_str().unwrap());
        assert_eq!(s.mode, SyncMode::Local);
        assert_eq!(s.sync, SyncSettings::default());
        assert!(s.sync.switch_on);
    }

    #[test]
    fn default_adapter_is_server() {
        let s = AppSettings::default();
        assert_eq!(s.adapter, AdapterKind::Server);
    }
}
