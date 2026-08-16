//! WebDAV 直连适配器：把引擎的 push/pull/next_event 映射为远端
//! `{base}/{dir}/{clipboard/current.json}` 的 PUT / GET / 轮询（Basic Auth）。

use std::time::Duration;

use async_trait::async_trait;
use xime_sync_domain::profile::Profile;

use super::CLIPBOARD_KEY;
use crate::transport::{Transport, TransportError};

/// WebDAV 直连传输通道。
pub struct WebDavTransport {
    /// 对象完整 URL（`{base}/{dir}/{clipboard/current.json}`）。
    url: String,
    auth: Option<String>,
    client: reqwest::Client,
}

impl WebDavTransport {
    /// `base_url` 为服务器地址（可含基础路径），`dir` 为同步子目录（可为空）。
    pub fn new(base_url: &str, dir: &str, username: &str, password: &str) -> Self {
        let mut base = base_url.trim_end_matches('/').to_string();
        let dir = dir.trim_matches('/');
        if !dir.is_empty() {
            base.push('/');
            base.push_str(dir);
        }
        let url = format!("{base}/{CLIPBOARD_KEY}");
        let auth = if username.is_empty() {
            None
        } else {
            Some(crate::transport::basic_auth_header(username, password))
        };
        Self {
            url,
            auth,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Transport for WebDavTransport {
    async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
        let body = serde_json::to_vec(profile).map_err(|e| TransportError::Send(e.to_string()))?;
        let mut req = self
            .client
            .put(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(auth) = &self.auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth);
        }
        let resp = req
            .body(body)
            .send()
            .await
            .map_err(|e| TransportError::Send(e.to_string()))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(TransportError::Send(format!("PUT {}", resp.status())))
        }
    }

    async fn pull(&self) -> Result<Option<Profile>, TransportError> {
        let mut req = self.client.get(&self.url);
        if let Some(auth) = &self.auth {
            req = req.header(reqwest::header::AUTHORIZATION, auth);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(TransportError::Recv(format!("GET {}", resp.status())));
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| TransportError::Recv(e.to_string()))
    }

    async fn next_event(&self, timeout: Duration) -> Result<Option<Profile>, TransportError> {
        tokio::time::sleep(timeout).await;
        self.pull().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_points_at_clipboard_key() {
        let t = WebDavTransport::new("https://dav.example.com/xime", "", "u", "p");
        assert_eq!(t.url, "https://dav.example.com/xime/clipboard/current.json");
        assert!(t.auth.is_some());
    }

    #[test]
    fn dir_is_joined_between_base_and_key() {
        let t = WebDavTransport::new("https://dav.example.com", "/sync/a/", "u", "p");
        assert_eq!(
            t.url,
            "https://dav.example.com/sync/a/clipboard/current.json"
        );
    }

    #[test]
    fn empty_dir_falls_back_to_base() {
        let t = WebDavTransport::new("https://dav.example.com/xime", "", "u", "p");
        assert_eq!(t.url, "https://dav.example.com/xime/clipboard/current.json");
    }
}
