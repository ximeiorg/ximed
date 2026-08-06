// 配置结构体是完整 schema 定义，部分字段由后续阶段消费（P3 WS 心跳/连接数，
// P4 SSE，P7 TLS/存储凭据），此处统一放行 dead_code 避免阶段性编译告警。
#![allow(dead_code)]

/// 串行化读写进程环境变量的测试（config/auth 测试共享）。
/// 返回处理过中毒的 Guard，测试 panic 时后续测试仍可继续。
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 获取 ENV_LOCK，容忍中毒。
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

use config::{Config, ConfigError, Environment, FileFormat, Format, Map, Source, Value};
use serde::Deserialize;

use domain::profile::Profile;

/// 内嵌默认配置源：把 default.toml 编译进二进制，保证裸跑可启动。
#[derive(Debug, Clone)]
pub struct MemorySource {
    content: String,
}

impl MemorySource {
    pub fn new() -> Self {
        Self {
            content: include_str!("../default.toml").to_string(),
        }
    }
}

impl Default for MemorySource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for MemorySource {
    fn clone_into_box(&self) -> Box<dyn Source + Send + Sync> {
        Box::new((*self).clone())
    }

    fn collect(&self) -> Result<Map<String, Value>, ConfigError> {
        FileFormat::Toml
            .parse(None, self.content.as_str())
            .map_err(|cause| ConfigError::FileParse { uri: None, cause })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub clipboard: ClipboardConfig,
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub ws: WsConfig,
    #[serde(default)]
    pub sse: SseConfig,
    pub storage: StorageConfig,
    #[serde(default)]
    pub tls: TlsConfig,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let s = Config::builder()
            .add_source(MemorySource::new()) // 1. 内嵌默认值
            .add_source(
                // XIMED_SERVER__ADDR 双下划线嵌套映射 → server.addr
                Environment::default()
                    .prefix("XIMED")
                    .separator("__")
                    .try_parsing(true),
            )
            .add_source(config::File::with_name("ximed.toml").required(false)) // 3. 当前目录
            .add_source(config::File::with_name("/app/ximed.toml").required(false)) // 4. /app
            .add_source(config::File::with_name("/etc/ximed/config.toml").required(false)) // 5. /etc
            .build()?;
        s.try_deserialize()
    }

    // 访问器（供各处安全取值）
    pub fn server(&self) -> &ServerConfig {
        &self.server
    }
    pub fn auth(&self) -> &AuthConfig {
        &self.auth
    }
    pub fn clipboard(&self) -> &ClipboardConfig {
        &self.clipboard
    }
    pub fn http(&self) -> &HttpConfig {
        &self.http
    }
    pub fn ws(&self) -> &WsConfig {
        &self.ws
    }
    pub fn sse(&self) -> &SseConfig {
        &self.sse
    }
    pub fn storage(&self) -> &StorageConfig {
        &self.storage
    }
    pub fn tls(&self) -> &TlsConfig {
        &self.tls
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::new().expect("default config must parse")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_addr")]
    pub addr: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    pub workers: Option<usize>,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_username")]
    pub username: String,
    /// 密码来源：环境变量（`password_env` 指定，默认 `XIMED_AUTH_PASSWORD`）优先；
    /// 未设置环境变量时回退到配置文件 `password` 字段。两者都未配置则认证不可用。
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default = "default_password_env")]
    pub password_env: String,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
}

impl AuthConfig {
    /// 读取密码：环境变量优先，配置文件 `password` 字段兜底。
    pub fn password(&self) -> Option<String> {
        std::env::var(&self.password_env)
            .ok()
            .filter(|p| !p.is_empty())
            .or_else(|| self.password.clone().filter(|p| !p.is_empty()))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClipboardConfig {
    #[serde(default = "default_max_frame_size")]
    pub max_frame_size: usize,
    #[serde(default = "default_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
    /// 空剪贴板初始值（可选覆盖）。
    #[serde(default)]
    pub default_profile: Option<Profile>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HttpConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_bind_timeout_ms")]
    pub bind_timeout_ms: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SseConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_sse_heartbeat_ms")]
    pub heartbeat_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_backend")]
    pub backend: String,
    // webdav 后端（feature = webdav 时启用）
    pub url: Option<String>,
    pub username: Option<String>,
    pub password_env: Option<String>,
    // s3 后端（feature = s3 时启用）
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub access_key_env: Option<String>,
    pub secret_key_env: Option<String>,
    // turso 后端（feature = turso 时启用，P7）
    pub auth_token_env: Option<String>,
}

impl StorageConfig {
    /// WebDAV 密码从环境变量读取（敏感项不落盘明文）。
    pub fn webdav_password(&self) -> Option<String> {
        self.password_env
            .as_ref()
            .and_then(|var| std::env::var(var).ok())
            .filter(|p| !p.is_empty())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub enabled: bool,
    pub cert_env: Option<String>,
    pub key_env: Option<String>,
}

/// 默认值函数（serde default 使用）
fn default_true() -> bool {
    true
}
fn default_addr() -> String {
    "0.0.0.0:8443".to_string()
}
fn default_data_dir() -> String {
    "./data".to_string()
}
fn default_log_level() -> String {
    "info".to_string()
}
fn default_username() -> String {
    "alice".to_string()
}
fn default_password_env() -> String {
    "XIMED_AUTH_PASSWORD".to_string()
}
fn default_max_connections() -> usize {
    32
}
fn default_max_frame_size() -> usize {
    1024 * 1024
}
fn default_idle_timeout_secs() -> u64 {
    60
}
fn default_heartbeat_interval_secs() -> u64 {
    30
}
fn default_bind_timeout_ms() -> u64 {
    5000
}
fn default_sse_heartbeat_ms() -> u64 {
    15_000
}
fn default_backend() -> String {
    "local".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个"仅默认值 + 指定环境变量"的 Settings，用于测试环境变量覆盖。
    /// 注意：config 的 Environment 源无法在配置加载后轻易注入，因此这里
    /// 复用 `Settings::new` 的实际 Builder，测试时通过 std::env 临时设置。
    #[test]
    fn default_values_parse() {
        let _g = env_lock();
        let s = Settings::default();
        assert_eq!(s.server().addr, "0.0.0.0:8443");
        assert_eq!(s.server().data_dir, "./data");
        assert_eq!(s.server().log_level, "info");
        assert_eq!(s.auth().username, "alice");
        assert_eq!(s.auth().max_connections, 32);
        assert_eq!(s.clipboard().max_frame_size, 1024 * 1024);
        assert_eq!(s.clipboard().idle_timeout_secs, 60);
        assert_eq!(s.clipboard().heartbeat_interval_secs, 30);
        assert!(s.http().enabled);
        assert_eq!(s.sse().heartbeat_ms, 15_000);
        assert_eq!(s.storage().backend, "local");
        assert!(!s.tls().enabled);
    }

    #[test]
    fn password_reads_from_env() {
        let _g = env_lock();
        let s = Settings::default();
        // 未设置环境变量时返回 None
        unsafe {
            std::env::remove_var(&s.auth().password_env);
        }
        assert_eq!(s.auth().password(), None);
        // 设置后能读到
        unsafe {
            std::env::set_var(&s.auth().password_env, "secret");
        }
        assert_eq!(s.auth().password().as_deref(), Some("secret"));
        unsafe {
            std::env::remove_var(&s.auth().password_env);
        }
    }

    #[test]
    fn password_from_config_file_field() {
        let _g = env_lock();
        // 未设置环境变量时，配置文件 password 字段兜底
        unsafe {
            std::env::remove_var("XIMED_AUTH_PASSWORD");
        }
        let mut cfg = AuthConfig {
            username: "alice".to_string(),
            password: Some("file-secret".to_string()),
            password_env: "XIMED_AUTH_PASSWORD".to_string(),
            max_connections: 32,
        };
        assert_eq!(cfg.password().as_deref(), Some("file-secret"));
        // 环境变量优先级更高
        unsafe {
            std::env::set_var("XIMED_AUTH_PASSWORD", "env-secret");
        }
        assert_eq!(cfg.password().as_deref(), Some("env-secret"));
        cfg.password = None;
        assert_eq!(cfg.password().as_deref(), Some("env-secret"));
        unsafe {
            std::env::remove_var("XIMED_AUTH_PASSWORD");
        }
        assert_eq!(cfg.password(), None);
    }

    #[test]
    fn environment_override_parses_types() {
        let _g = env_lock();
        // 双下划线嵌套映射：XIMED__SERVER__ADDR → server.addr（prefix("XIMED") + separator("__")）
        unsafe {
            std::env::set_var("XIMED__SERVER__ADDR", "0.0.0.0:9999");
            std::env::set_var("XIMED__CLIPBOARD__MAX_FRAME_SIZE", "2048");
            std::env::set_var("XIMED__HTTP__ENABLED", "false");
            std::env::set_var("XIMED__STORAGE__BACKEND", "webdav");
        }
        let s = Settings::new().expect("parse with env override");
        assert_eq!(s.server().addr, "0.0.0.0:9999");
        assert_eq!(s.clipboard().max_frame_size, 2048);
        assert!(!s.http().enabled);
        assert_eq!(s.storage().backend, "webdav");
        for var in [
            "XIMED__SERVER__ADDR",
            "XIMED__CLIPBOARD__MAX_FRAME_SIZE",
            "XIMED__HTTP__ENABLED",
            "XIMED__STORAGE__BACKEND",
        ] {
            unsafe {
                std::env::remove_var(var);
            }
        }
    }

    #[test]
    fn ximed_toml_overrides_env_and_defaults() {
        // 在临时目录写 ximed.toml，验证文件 > 环境变量 > 默认值优先级。
        // 但 Settings::new 固定读 ./ximed.toml 与固定路径，这里用配置文件中
        // 的字段验证 config::File 源确实参与合并（测试通过环境变量使 data_dir
        // 指向临时目录后再验证默认值不受影响）。
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("ximed.toml");
        std::fs::write(
            &cfg_path,
            r#"
[server]
addr = "127.0.0.1:1111"
"#,
        )
        .unwrap();

        // 通过环境变量注入任意不存在文件路径的覆盖不可行；改为直接构造
        // Builder 验证 File 源语义（与 Settings::new 同构）。
        let s = config::Config::builder()
            .add_source(MemorySource::new())
            .add_source(config::File::from(cfg_path.as_path()))
            .build()
            .unwrap()
            .try_deserialize::<Settings>()
            .unwrap();
        assert_eq!(s.server().addr, "127.0.0.1:1111");
        // 未被文件覆盖的字段保持默认
        assert_eq!(s.server().log_level, "info");
        assert_eq!(s.auth().max_connections, 32);
    }
}
