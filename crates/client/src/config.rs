//! client 配置：服务器地址、认证、轮询间隔。
//!
//! 由 `app` crate 从 CLI / 环境变量 / 配置文件构建。敏感项（密码）走环境变量。
//! 复用 server 的 config-rs 范式：内嵌默认 + 环境变量 + 多路径文件。

use serde::{Deserialize, Serialize};

/// 同步适配器类型（client 直连后端，对标 SyncClipboard adapter）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    /// 连接 ximed server（默认，HTTP/WS 协议通道）。
    #[default]
    Server,
    /// 直连 WebDAV 存储（绕过 ximed server）。
    WebDav,
    /// 直连 S3 兼容存储（绕过 ximed server）。
    S3,
}

/// client 配置。
#[derive(Debug, Clone, Deserialize)]
pub struct ClientConfig {
    /// 服务器基础地址，如 `http://127.0.0.1:8443` 或 `https://example.com`。
    /// WS 通道自动推导为 `ws(s)://host:port/sync`。
    #[serde(default)]
    pub server_url: String,
    /// Basic Auth 用户名。
    #[serde(default)]
    pub username: String,
    /// 密码来源：环境变量（`password_env` 指定，默认 `XIMED_CLIENT_PASSWORD`）优先；
    /// 未设置环境变量时回退到配置文件 `password` 字段。两者都未配置则启动失败。
    #[serde(default)]
    pub password: Option<String>,
    /// Basic Auth 密码（敏感项，通过环境变量注入，默认 `XIMED_CLIENT_PASSWORD`）。
    #[serde(default = "default_password_env")]
    pub password_env: String,
    /// 本机设备标识（写入 profile.source）。
    #[serde(default = "default_source")]
    pub source: String,
    /// 本地剪贴板轮询间隔（毫秒）。
    #[serde(default = "default_poll_ms")]
    pub poll_ms: u64,
    /// WS 断线后 HTTP 轮询间隔（毫秒）。
    #[serde(default = "default_poll_ms")]
    pub http_fallback_ms: u64,
    /// WS 心跳间隔（秒），空闲时向服务器发 ping。
    #[serde(default = "default_heartbeat_secs")]
    pub heartbeat_secs: u64,
    /// 是否启用 WS 实时通道（false 则纯 HTTP 轮询）。
    #[serde(default = "default_true")]
    pub ws_enabled: bool,
    /// 同步适配器：server（默认）/ webdav / s3。
    #[serde(default)]
    pub adapter: AdapterKind,
    /// WebDAV 直连：服务基础地址（如 `https://dav.example.com`）。
    #[serde(default)]
    pub webdav_url: String,
    /// WebDAV 直连：同步子目录（如 `xime`，可空；与 webdav_url 拼接）。
    #[serde(default)]
    pub webdav_dir: String,
    /// WebDAV 直连：用户名。
    #[serde(default)]
    pub webdav_username: String,
    /// WebDAV 直连：密码来源环境变量名（默认 `XIMED_WEBDAV_PASSWORD`）。
    #[serde(default = "default_webdav_password_env")]
    pub webdav_password_env: String,
    /// WebDAV 直连：密码明文兜底（desktop 设置面板场景；环境变量优先）。
    #[serde(default)]
    pub webdav_password: Option<String>,
    /// S3 直连：端点（path-style，如 `https://s3.example.com`）。
    #[serde(default)]
    pub s3_endpoint: String,
    /// S3 直连：存储桶名。
    #[serde(default)]
    pub s3_bucket: String,
    /// S3 直连：区域。
    #[serde(default = "default_s3_region")]
    pub s3_region: String,
    /// S3 直连：Access Key 来源环境变量名（默认 `XIMED_S3_ACCESS_KEY`）。
    #[serde(default = "default_s3_access_key_env")]
    pub s3_access_key_env: String,
    /// S3 直连：Secret Key 来源环境变量名（默认 `XIMED_S3_SECRET_KEY`）。
    #[serde(default = "default_s3_secret_key_env")]
    pub s3_secret_key_env: String,
    /// S3 直连：Access Key 明文兜底（desktop 设置面板场景；环境变量优先）。
    #[serde(default)]
    pub s3_access_key: Option<String>,
    /// S3 直连：Secret Key 明文兜底（desktop 设置面板场景；环境变量优先）。
    #[serde(default)]
    pub s3_secret_key: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:8443".to_string(),
            username: "alice".to_string(),
            password: None,
            password_env: default_password_env(),
            source: default_source(),
            poll_ms: default_poll_ms(),
            http_fallback_ms: default_poll_ms(),
            heartbeat_secs: default_heartbeat_secs(),
            ws_enabled: default_true(),
            adapter: AdapterKind::default(),
            webdav_url: String::new(),
            webdav_dir: String::new(),
            webdav_username: String::new(),
            webdav_password_env: default_webdav_password_env(),
            webdav_password: None,
            s3_endpoint: String::new(),
            s3_bucket: String::new(),
            s3_region: default_s3_region(),
            s3_access_key_env: default_s3_access_key_env(),
            s3_secret_key_env: default_s3_secret_key_env(),
            s3_access_key: None,
            s3_secret_key: None,
        }
    }
}

impl ClientConfig {
    /// 读取密码：环境变量优先，配置文件 `password` 字段兜底。
    pub fn password(&self) -> Option<String> {
        std::env::var(&self.password_env)
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| self.password.clone().filter(|p| !p.is_empty()))
    }

    /// 读取 WebDAV 直连密码：环境变量优先，配置字段明文兜底。
    pub fn webdav_password(&self) -> Option<String> {
        std::env::var(&self.webdav_password_env)
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| self.webdav_password.clone().filter(|p| !p.is_empty()))
    }

    /// 读取 S3 Access Key：环境变量优先，配置字段明文兜底。
    pub fn s3_access_key(&self) -> Option<String> {
        std::env::var(&self.s3_access_key_env)
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.s3_access_key.clone().filter(|s| !s.is_empty()))
    }

    /// 读取 S3 Secret Key：环境变量优先，配置字段明文兜底。
    pub fn s3_secret_key(&self) -> Option<String> {
        std::env::var(&self.s3_secret_key_env)
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.s3_secret_key.clone().filter(|s| !s.is_empty()))
    }

    /// 推导 WS 地址：`http(s)://host:port` → `ws(s)://host:port/sync`。
    pub fn ws_url(&self) -> String {
        let base = self.server_url.trim_end_matches('/');
        if let Some(rest) = base.strip_prefix("https") {
            format!("wss{rest}/sync")
        } else if let Some(rest) = base.strip_prefix("http") {
            format!("ws{rest}/sync")
        } else {
            format!("{base}/sync")
        }
    }

    /// 推导 HTTP API 基地址（剪贴板端点）。
    pub fn http_api(&self) -> String {
        format!("{}/api/clipboard", self.server_url.trim_end_matches('/'))
    }

    /// 从环境变量 + 配置文件加载（config-rs 范式，优先级：默认 < env < 文件）。
    ///
    /// - 环境变量：`XIMED_CLIENT_SERVER_URL`、`XIMED_CLIENT_USERNAME`、
    ///   `XIMED_CLIENT_PASSWORD_ENV`、`XIMED_CLIENT_SOURCE`、`XIMED_CLIENT_POLL_MS`…
    /// - 配置文件：`ximed.toml` 的 `[client]` 节（`server_url` 等字段同名）。
    pub fn load() -> Result<Self, config::ConfigError> {
        let s = config::Config::builder()
            .add_source(config::Environment::with_prefix("XIMED_CLIENT").try_parsing(true))
            .add_source(config::File::with_name("ximed.toml").required(false))
            .add_source(config::File::with_name("/app/ximed.toml").required(false))
            .add_source(config::File::with_name("/etc/ximed/config.toml").required(false))
            .build()?;
        let mut cfg: Self = s.get::<Self>("client").unwrap_or_default();
        // 环境变量兜底：prefix 模式不含 [client] 包装，直接覆盖顶层字段
        if let Ok(v) = std::env::var("XIMED_CLIENT_SERVER_URL") {
            cfg.server_url = v;
        }
        if let Ok(v) = std::env::var("XIMED_CLIENT_USERNAME") {
            cfg.username = v;
        }
        if let Ok(v) = std::env::var("XIMED_CLIENT_SOURCE") {
            cfg.source = v;
        }
        Ok(cfg)
    }
}

fn default_password_env() -> String {
    "XIMED_CLIENT_PASSWORD".to_string()
}
fn default_webdav_password_env() -> String {
    "XIMED_WEBDAV_PASSWORD".to_string()
}
fn default_s3_region() -> String {
    "us-east-1".to_string()
}
fn default_s3_access_key_env() -> String {
    "XIMED_S3_ACCESS_KEY".to_string()
}
fn default_s3_secret_key_env() -> String {
    "XIMED_S3_SECRET_KEY".to_string()
}
fn default_source() -> String {
    hostname().unwrap_or_else(|| "client".to_string())
}
fn default_poll_ms() -> u64 {
    500
}
fn default_heartbeat_secs() -> u64 {
    30
}
fn default_true() -> bool {
    true
}

/// 本机主机名（来源标识）。
pub fn hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_derived_from_http() {
        let cfg = ClientConfig {
            server_url: "http://127.0.0.1:8443".to_string(),
            ..ClientConfig::default()
        };
        assert_eq!(cfg.ws_url(), "ws://127.0.0.1:8443/sync");
        assert_eq!(cfg.http_api(), "http://127.0.0.1:8443/api/clipboard");

        let tls = ClientConfig {
            server_url: "https://example.com".to_string(),
            ..cfg
        };
        assert_eq!(tls.ws_url(), "wss://example.com/sync");
        assert_eq!(tls.http_api(), "https://example.com/api/clipboard");
    }

    #[test]
    fn password_reads_from_env() {
        let _g = env_lock();
        unsafe {
            std::env::set_var("XIMED_CLIENT_PASSWORD", "secret");
        }
        let cfg = ClientConfig {
            server_url: "http://x".to_string(),
            ..ClientConfig::default()
        };
        assert_eq!(cfg.password().as_deref(), Some("secret"));
        unsafe {
            std::env::remove_var("XIMED_CLIENT_PASSWORD");
        }
    }

    #[test]
    fn password_from_config_file_field() {
        let _g = env_lock();
        unsafe {
            std::env::remove_var("XIMED_CLIENT_PASSWORD");
        }
        let cfg = ClientConfig {
            server_url: "http://x".to_string(),
            password: Some("file-secret".to_string()),
            ..ClientConfig::default()
        };
        assert_eq!(cfg.password().as_deref(), Some("file-secret"));
        // 环境变量优先级更高
        unsafe {
            std::env::set_var("XIMED_CLIENT_PASSWORD", "env-secret");
        }
        assert_eq!(cfg.password().as_deref(), Some("env-secret"));
        unsafe {
            std::env::remove_var("XIMED_CLIENT_PASSWORD");
        }
    }

    #[test]
    fn direct_adapter_defaults() {
        let cfg = ClientConfig::default();
        assert_eq!(cfg.adapter, AdapterKind::Server);
        assert_eq!(cfg.webdav_password_env, "XIMED_WEBDAV_PASSWORD");
        assert_eq!(cfg.s3_region, "us-east-1");
        assert_eq!(cfg.s3_access_key_env, "XIMED_S3_ACCESS_KEY");
        assert_eq!(cfg.s3_secret_key_env, "XIMED_S3_SECRET_KEY");
    }

    #[test]
    fn direct_credentials_read_from_env() {
        let _g = env_lock();
        unsafe {
            std::env::set_var("XIMED_WEBDAV_PASSWORD", "dav-secret");
            std::env::set_var("XIMED_S3_ACCESS_KEY", "AKIA123");
            std::env::set_var("XIMED_S3_SECRET_KEY", "s3-secret");
        }
        let cfg = ClientConfig::default();
        assert_eq!(cfg.webdav_password().as_deref(), Some("dav-secret"));
        assert_eq!(cfg.s3_access_key().as_deref(), Some("AKIA123"));
        assert_eq!(cfg.s3_secret_key().as_deref(), Some("s3-secret"));
        unsafe {
            std::env::remove_var("XIMED_WEBDAV_PASSWORD");
            std::env::remove_var("XIMED_S3_ACCESS_KEY");
            std::env::remove_var("XIMED_S3_SECRET_KEY");
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}
