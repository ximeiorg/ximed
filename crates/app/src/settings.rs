//! 应用级设置（desktop 形态）：client 连接本机 server 或远程 server。
//!
//! 持久化到 `{config_dir}/ximed-client.toml`，与 server 的 `ximed.toml` 分离，避免互相干扰。

use serde::{Deserialize, Serialize};

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
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            mode: SyncMode::Local,
            remote_url: String::new(),
            remote_username: String::new(),
            remote_password: None,
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
            mode: SyncMode::Remote,
            remote_url: "https://example.com:8443".to_string(),
            remote_username: "alice".to_string(),
            remote_password: Some("secret".to_string()),
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
}
