use axum::http::HeaderMap;
use base64::Engine;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::AuthConfig;

/// 从 `Authorization: Basic base64(user:pass)` 解析凭据，返回 (user, pass)。
pub fn parse_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let encoded = value.strip_prefix("Basic ")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    let decoded = std::str::from_utf8(&bytes).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

/// 恒定时间比较两个字符串（先 SHA256 摘要再 ct_eq，避免长度/字节差异时序泄漏）。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let da = Sha256::digest(a.as_bytes());
    let db = Sha256::digest(b.as_bytes());
    bool::from(da.ct_eq(&db))
}

/// 校验 Basic Auth 头是否匹配配置的用户名/密码。
pub fn check_basic_auth(headers: &HeaderMap, auth: &AuthConfig) -> bool {
    let Some((user, pass)) = parse_basic_auth(headers) else {
        return false;
    };
    let Some(expected_pass) = auth.password() else {
        return false;
    };
    user == auth.username && constant_time_eq(&pass, &expected_pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_header(user: &str, pass: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let raw = format!("{}:{}", user, pass);
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Basic {encoded}").parse().unwrap(),
        );
        headers
    }

    fn test_auth_config() -> AuthConfig {
        crate::config::AuthConfig {
            username: "alice".to_string(),
            password: None,
            password_env: "XIMED_AUTH_PASSWORD".to_string(),
            max_connections: 32,
        }
    }

    #[test]
    fn parse_valid_credentials() {
        let headers = auth_header("alice", "s3cret");
        let (user, pass) = parse_basic_auth(&headers).unwrap();
        assert_eq!(user, "alice");
        assert_eq!(pass, "s3cret");
    }

    #[test]
    fn parse_missing_header() {
        assert_eq!(parse_basic_auth(&HeaderMap::new()), None);
    }

    #[test]
    fn parse_wrong_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer xyz".parse().unwrap(),
        );
        assert_eq!(parse_basic_auth(&headers), None);
    }

    #[test]
    fn parse_invalid_base64() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Basic !!!not-base64!!!".parse().unwrap(),
        );
        assert_eq!(parse_basic_auth(&headers), None);
    }

    #[test]
    fn check_matches_config() {
        let _g = crate::config::env_lock();
        let cfg = test_auth_config();
        unsafe {
            std::env::set_var(&cfg.password_env, "s3cret");
        }
        assert!(check_basic_auth(&auth_header("alice", "s3cret"), &cfg));
        assert!(!check_basic_auth(&auth_header("alice", "wrong"), &cfg));
        assert!(!check_basic_auth(&auth_header("bob", "s3cret"), &cfg));
        assert!(!check_basic_auth(&HeaderMap::new(), &cfg));
        unsafe {
            std::env::remove_var(&cfg.password_env);
        }
    }

    #[test]
    fn check_fails_when_password_env_unset() {
        let _g = crate::config::env_lock();
        let cfg = test_auth_config();
        unsafe {
            std::env::remove_var(&cfg.password_env);
        }
        assert!(!check_basic_auth(&auth_header("alice", "s3cret"), &cfg));
    }
}
