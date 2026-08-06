//! client 配置：服务器地址、认证、轮询间隔。
//!
//! 由 `app` crate 从 CLI / 环境变量 / 配置文件构建。敏感项（密码）走环境变量。
//! 复用 server 的 config-rs 范式：内嵌默认 + 环境变量 + 多路径文件。

use serde::Deserialize;

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

fn default_password_env() -> String { "XIMED_CLIENT_PASSWORD".to_string() }
fn default_source() -> String { hostname().unwrap_or_else(|| "client".to_string()) }
fn default_poll_ms() -> u64 { 500 }
fn default_heartbeat_secs() -> u64 { 30 }
fn default_true() -> bool { true }

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
            username: "alice".to_string(),
            password: None,
            password_env: "XIMED_CLIENT_PASSWORD".to_string(),
            source: "t".to_string(),
            poll_ms: 500,
            http_fallback_ms: 500,
            heartbeat_secs: 30,
            ws_enabled: true,
        };
        assert_eq!(cfg.ws_url(), "ws://127.0.0.1:8443/sync");
        assert_eq!(cfg.http_api(), "http://127.0.0.1:8443/api/clipboard");

        let tls = ClientConfig { server_url: "https://example.com".to_string(), ..cfg };
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
            username: "u".to_string(),
            password: None,
            password_env: "XIMED_CLIENT_PASSWORD".to_string(),
            source: "t".to_string(),
            poll_ms: 500,
            http_fallback_ms: 500,
            heartbeat_secs: 30,
            ws_enabled: true,
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
            username: "u".to_string(),
            password: Some("file-secret".to_string()),
            password_env: "XIMED_CLIENT_PASSWORD".to_string(),
            source: "t".to_string(),
            poll_ms: 500,
            http_fallback_ms: 500,
            heartbeat_secs: 30,
            ws_enabled: true,
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

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}
