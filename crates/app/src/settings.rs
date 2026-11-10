//! 应用级设置（desktop 形态）：通道优先级 + 每条通道自己的参数 + 同步内容。
//!
//! 持久化到 `{config_dir}/ximed-client.toml`，与 server 的 `ximed.toml` 分离，避免互相干扰。
//!
//! # 配置形状
//!
//! ```toml
//! channels = ["p2p", "webdav"]        # 顺序即优先级，首个优先
//!
//! [http]                              # ximed server（界面默认不暴露）
//! mode = "local"                      # local | remote
//! url = ""                            # mode = remote 时生效
//! username = ""
//! password = ""
//!
//! [webdav]
//! url = "https://dav.example.com/dav/ximed"
//! file = "clipboard.json"
//! username = "alice"
//! password = ""
//!
//! [p2p]
//! device_name = "办公室 Mac"
//!
//! [sync]
//! switch_on = true
//! ```
//!
//! 旧版是扁平的 `mode` / `remote_url` / `remote_username` / `remote_password`：
//! [`AppSettings::load`] 会把它们搬进 `[http]`，并在下次保存时写成新形状。

use serde::{Deserialize, Serialize};

use client::config::{P2pSettings, WebDavSettings};
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

/// HTTP 通道（ximed server）设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpSettings {
    #[serde(default)]
    pub mode: SyncMode,
    /// 远程服务器基础地址，如 `https://example.com:8443`（`mode = remote` 时生效）。
    #[serde(default)]
    pub url: String,
    /// 远程服务器用户名。
    #[serde(default)]
    pub username: String,
    /// 远程服务器密码（敏感项明文落盘；desktop 单机场景可接受）。
    #[serde(default)]
    pub password: Option<String>,
}

impl Default for HttpSettings {
    fn default() -> Self {
        Self {
            mode: SyncMode::Local,
            url: String::new(),
            username: String::new(),
            password: None,
        }
    }
}

/// 应用级设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// 通道优先级列表（首个优先）。空 = 默认 HTTP 通道。
    #[serde(default)]
    pub channels: Vec<String>,
    /// HTTP 通道参数。
    #[serde(default)]
    pub http: HttpSettings,
    /// WebDAV 通道参数。
    #[serde(default)]
    pub webdav: WebDavSettings,
    /// 局域网直连参数（`data_dir` 由宿主在运行时补齐，不落盘）。
    #[serde(default)]
    pub p2p: P2pSettings,
    /// 同步设置（定义在 client crate，app → client 依赖方向）。
    #[serde(default)]
    pub sync: SyncSettings,
    /// 旧版扁平字段：只读，用于迁移，不再写回。
    #[serde(flatten, default, skip_serializing)]
    legacy: LegacyHttp,
}

/// 旧版扁平 HTTP 字段（v0 配置形状）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct LegacyHttp {
    #[serde(default)]
    mode: Option<SyncMode>,
    #[serde(default)]
    remote_url: Option<String>,
    #[serde(default)]
    remote_username: Option<String>,
    #[serde(default)]
    remote_password: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            channels: client::channel::default_channels(),
            http: HttpSettings::default(),
            webdav: WebDavSettings::default(),
            p2p: P2pSettings::default(),
            sync: SyncSettings::default(),
            legacy: LegacyHttp::default(),
        }
    }
}

impl AppSettings {
    /// 从 `{config_dir}/ximed-client.toml` 加载；文件缺失或解析失败回退默认值。
    pub fn load(config_dir: &str) -> Self {
        let path = std::path::Path::new(config_dir).join("ximed-client.toml");
        let parsed = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str::<Self>(&s).ok());
        match parsed {
            Some(s) => s.migrated(),
            None => Self::default(),
        }
    }

    /// 把旧版扁平字段搬进 `[http]`，并规范化通道列表。
    fn migrated(mut self) -> Self {
        let http_is_empty = self.http.url.is_empty()
            && self.http.username.is_empty()
            && self.http.password.is_none()
            && self.http.mode == SyncMode::Local;
        if http_is_empty {
            if let Some(mode) = self.legacy.mode {
                self.http.mode = mode;
            }
            if let Some(url) = self.legacy.remote_url.clone() {
                self.http.url = url;
            }
            if let Some(user) = self.legacy.remote_username.clone() {
                self.http.username = user;
            }
            self.http.password = self.legacy.remote_password.clone();
        }
        self.legacy = LegacyHttp::default();
        self.channels = normalized(&self.channels);
        self
    }

    /// 保存到 `{config_dir}/ximed-client.toml`。
    pub fn save(&self, config_dir: &str) -> std::io::Result<()> {
        let dir = std::path::Path::new(config_dir);
        std::fs::create_dir_all(dir)?;
        let mut clean = self.clone();
        clean.legacy = LegacyHttp::default();
        clean.channels = normalized(&clean.channels);
        let text =
            toml::to_string_pretty(&clean).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(dir.join("ximed-client.toml"), text)
    }

    /// 通道列表规范化；空列表退回默认 HTTP 通道（保证同步不因配置为空而停摆）。
    pub fn normalized_channels(&self) -> Vec<String> {
        normalized(&self.channels)
    }
}

fn normalized(channels: &[String]) -> Vec<String> {
    let list = client::channel::normalize(channels);
    if list.is_empty() {
        client::channel::default_channels()
    } else {
        list
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
            channels: vec!["webdav".to_string(), "http".to_string()],
            http: HttpSettings {
                mode: SyncMode::Remote,
                url: "https://example.com:8443".to_string(),
                username: "alice".to_string(),
                password: Some("secret".to_string()),
            },
            webdav: WebDavSettings {
                url: "https://dav.example.com/dav/ximed".to_string(),
                username: "bob".to_string(),
                ..WebDavSettings::default()
            },
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
            ..AppSettings::default()
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
        assert_eq!(s.http.mode, SyncMode::Local);
        assert_eq!(s.sync, SyncSettings::default());
        assert!(s.sync.switch_on);
    }

    /// 旧配置（扁平 mode/remote_*）必须迁移到 [http]，而不是被忽略后回落本机。
    #[test]
    fn legacy_flat_http_fields_are_migrated() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("ximed-client.toml"),
            r#"
mode = "remote"
remote_url = "https://hub.example.com"
remote_username = "alice"
remote_password = "secret"
"#,
        )
        .unwrap();
        let s = AppSettings::load(dir.path().to_str().unwrap());
        assert_eq!(s.http.mode, SyncMode::Remote);
        assert_eq!(s.http.url, "https://hub.example.com");
        assert_eq!(s.http.username, "alice");
        assert_eq!(s.http.password.as_deref(), Some("secret"));
        // 没配过通道 → 沿用默认 HTTP，不中断同步
        assert_eq!(s.channels, client::channel::default_channels());

        // 保存后应写成新形状，旧字段不再出现
        s.save(dir.path().to_str().unwrap()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("ximed-client.toml")).unwrap();
        assert!(text.contains("[http]"), "应写成 [http] 节：{text}");
        assert!(!text.contains("remote_url"), "旧字段不应再写回：{text}");
    }

    #[test]
    fn unknown_channels_are_dropped_on_load() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("ximed-client.toml"),
            "channels = [\"com.example.sync\", \"webdav\"]\n",
        )
        .unwrap();
        let s = AppSettings::load(dir.path().to_str().unwrap());
        assert_eq!(s.channels, vec!["webdav".to_string()]);
    }

    /// 通道列表全非法或为空时退回默认 HTTP：否则保存一次就会把同步关掉。
    #[test]
    fn empty_channels_fall_back_to_http() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("ximed-client.toml"),
            "channels = [\"com.example.sync\"]\n",
        )
        .unwrap();
        let s = AppSettings::load(dir.path().to_str().unwrap());
        assert_eq!(s.channels, client::channel::default_channels());
        assert_eq!(s.normalized_channels(), vec!["http".to_string()]);
    }
}
