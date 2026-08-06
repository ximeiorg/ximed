use axum_server::tls_rustls::RustlsConfig;

use crate::config::TlsConfig;

/// TLS 错误。
#[derive(Debug)]
pub enum TlsError {
    /// 证书环境变量未设置。
    EnvMissing(String),
    /// PEM 解析失败。
    Parse(String),
}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TlsError::EnvMissing(var) => write!(f, "tls env var {var} not set"),
            TlsError::Parse(msg) => write!(f, "tls parse error: {msg}"),
        }
    }
}

impl std::error::Error for TlsError {}

/// 从环境变量读取 PEM 证书与私钥，构建 axum-server 的 RustlsConfig。
///
/// 参考 axum 官方 example-tls-rustls：证书/私钥经 PEM 构建 TLS 配置。
/// 差异：证书不落盘，直接经环境变量注入（设计文档 5.6「敏感项不落盘明文」）。
pub async fn build_rustls_config(cfg: &TlsConfig) -> Result<RustlsConfig, TlsError> {
    let cert_var = cfg
        .cert_env
        .as_ref()
        .ok_or_else(|| TlsError::EnvMissing("cert_env".into()))?;
    let key_var = cfg
        .key_env
        .as_ref()
        .ok_or_else(|| TlsError::EnvMissing("key_env".into()))?;

    let cert_pem = std::env::var(cert_var).map_err(|_| TlsError::EnvMissing(cert_var.clone()))?;
    let key_pem = std::env::var(key_var).map_err(|_| TlsError::EnvMissing(key_var.clone()))?;

    RustlsConfig::from_pem(cert_pem.into_bytes(), key_pem.into_bytes())
        .await
        .map_err(|e| TlsError::Parse(e.to_string()))
}
