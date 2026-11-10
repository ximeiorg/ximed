//! client 配置：服务器地址、认证、轮询间隔。
//!
//! 由 `app` crate 从 CLI / 环境变量 / 配置文件构建。敏感项（密码）走环境变量。
//! 复用 server 的 config-rs 范式：内嵌默认 + 环境变量 + 多路径文件。

use serde::{Deserialize, Serialize};

/// 同步适配器类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterKind {
    /// 连接 ximed server（默认，HTTP/WS 协议通道）。
    #[default]
    Server,
    /// 同步插件（Lua clipboard_sync 插件，需 client `plugin` feature）。
    Plugin,
}

/// WebDAV 通道设置：把当前剪贴板当作网盘上的一个 JSON 文件。
///
/// 语义与 HTTP 通道一致（单值、最后写入胜出），只是载体换成用户自己的网盘：
/// 不依赖 ximed server，也不需要插件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebDavSettings {
    /// 网盘上的目录地址，如 `https://dav.jianguoyun.com/dav/ximed`。
    #[serde(default)]
    pub url: String,
    /// 目录下的文件名（相对路径），默认 `clipboard.json`。
    #[serde(default = "default_webdav_file")]
    pub file: String,
    /// Basic Auth 用户名。
    #[serde(default)]
    pub username: String,
    /// 密码来源环境变量；未设置时回退 `password` 字段。
    #[serde(default = "default_webdav_password_env")]
    pub password_env: String,
    /// 密码（敏感项，优先走环境变量）。
    #[serde(default)]
    pub password: Option<String>,
}

impl Default for WebDavSettings {
    fn default() -> Self {
        Self {
            url: String::new(),
            file: default_webdav_file(),
            username: String::new(),
            password_env: default_webdav_password_env(),
            password: None,
        }
    }
}

impl WebDavSettings {
    /// 目标文件完整地址：目录末尾斜杠与文件名开头斜杠都不会造成双斜杠。
    pub fn endpoint(&self) -> String {
        let base = self.url.trim().trim_end_matches('/');
        let file = self.file.trim().trim_start_matches('/');
        if file.is_empty() {
            base.to_string()
        } else {
            format!("{base}/{file}")
        }
    }

    /// 目标文件所在目录（WebDAV 的 PROPFIND 要打在目录上）。
    pub fn dir_endpoint(&self) -> String {
        self.url.trim().trim_end_matches('/').to_string()
    }

    /// 读取密码：环境变量优先，配置文件字段兜底。
    pub fn password(&self) -> Option<String> {
        std::env::var(&self.password_env)
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| self.password.clone().filter(|p| !p.is_empty()))
    }

    /// 是否配置完整到可以启用（地址与文件名非空）。
    pub fn is_configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.file.trim().is_empty()
    }
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
    /// 同步适配器：server（默认）/ plugin。
    #[serde(default)]
    pub adapter: AdapterKind,
    /// 同步插件根目录（adapter=plugin 时生效；`<root>/config/<id>.yaml` 为插件配置）。
    #[serde(default)]
    pub plugins_dir: String,
    /// 激活的同步插件 id（adapter=plugin 时生效）。
    #[serde(default)]
    pub plugin_id: String,
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
            plugins_dir: String::new(),
            plugin_id: String::new(),
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

fn default_password_env() -> String {
    "XIMED_CLIENT_PASSWORD".to_string()
}
fn default_webdav_file() -> String {
    "clipboard.json".to_string()
}
fn default_webdav_password_env() -> String {
    "XIMED_WEBDAV_PASSWORD".to_string()
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
        assert_eq!(cfg.plugins_dir, "");
        assert_eq!(cfg.plugin_id, "");
    }

    #[test]
    fn plugin_adapter_deserializes_from_config_file() {
        // 旧配置文件无 plugin 字段 → 默认；新配置可反序列化 plugin 适配器
        let legacy: ClientConfig = serde_json::from_str(r#"{"server_url": "http://x"}"#).unwrap();
        assert_eq!(legacy.adapter, AdapterKind::Server);
        assert_eq!(legacy.plugins_dir, "");

        let plugin: ClientConfig = serde_json::from_str(
            r#"{"adapter": "plugin", "plugins_dir": "/cfg/plugins", "plugin_id": "com.example.sync"}"#,
        )
        .unwrap();
        assert_eq!(plugin.adapter, AdapterKind::Plugin);
        assert_eq!(plugin.plugins_dir, "/cfg/plugins");
        assert_eq!(plugin.plugin_id, "com.example.sync");
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}
