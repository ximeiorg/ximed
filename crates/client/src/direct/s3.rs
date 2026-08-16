//! S3 直连适配器：把引擎的 push/pull/next_event 映射为 S3 对象
//! `{endpoint}/{bucket}/{clipboard/current.json}` 的 PUT / GET / 轮询（SigV4 签名）。

use std::time::Duration;

use async_trait::async_trait;
use xime_sync_domain::profile::Profile;

use super::CLIPBOARD_KEY;
use super::sigv4::{empty_payload_hash, sign_request};
use crate::transport::{Transport, TransportError};

/// S3 直连传输通道（path-style 访问）。
pub struct S3Transport {
    /// 对象 URL（`{endpoint}/{bucket}/{key}`）。
    url: String,
    /// canonical URI（`/bucket/{key}`），签名用。
    canonical_uri: String,
    /// 请求 host 头（endpoint 的 host[:port]）。
    host: String,
    region: String,
    access_key: String,
    secret_key: String,
    client: reqwest::Client,
}

impl S3Transport {
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
    ) -> Self {
        let endpoint = endpoint.trim_end_matches('/');
        let host = url_host(endpoint);
        let canonical_uri = format!("/{bucket}/{CLIPBOARD_KEY}");
        let url = format!("{endpoint}/{bucket}/{CLIPBOARD_KEY}");
        Self {
            url,
            canonical_uri,
            host,
            region: region.to_string(),
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn signed_request(
        &self,
        method: &str,
        body: Option<&[u8]>,
    ) -> Result<reqwest::RequestBuilder, TransportError> {
        let payload_hash = match body {
            Some(data) => {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(data);
                hex::encode(hasher.finalize())
            }
            None => empty_payload_hash(),
        };
        let sig = sign_request(
            method,
            &self.host,
            &self.canonical_uri,
            &payload_hash,
            &self.access_key,
            &self.secret_key,
            &self.region,
            "s3",
        );
        let req = self
            .client
            .request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|e| TransportError::Send(e.to_string()))?,
                &self.url,
            )
            .header("x-amz-date", &sig.amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .header(reqwest::header::AUTHORIZATION, &sig.authorization);
        if let Some(data) = body {
            return Ok(req
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(data.to_vec()));
        }
        Ok(req)
    }
}

/// 从 endpoint URL 提取 host（含端口，不含 scheme/路径）。
fn url_host(endpoint: &str) -> String {
    let rest = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint);
    match rest.find('/') {
        Some(i) => rest[..i].to_string(),
        None => rest.to_string(),
    }
}

#[async_trait]
impl Transport for S3Transport {
    async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
        let body = serde_json::to_vec(profile).map_err(|e| TransportError::Send(e.to_string()))?;
        let resp = self
            .signed_request("PUT", Some(&body))?
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
        let resp = self
            .signed_request("GET", None)?
            .send()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(TransportError::Recv(format!("GET {}", resp.status())));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        serde_json::from_slice(&bytes)
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
    fn url_and_host_parsed() {
        let t = S3Transport::new(
            "https://s3.example.com",
            "clipboard-bucket",
            "us-east-1",
            "AKIA",
            "secret",
        );
        assert_eq!(
            t.url,
            "https://s3.example.com/clipboard-bucket/clipboard/current.json"
        );
        assert_eq!(t.canonical_uri, "/clipboard-bucket/clipboard/current.json");
        assert_eq!(t.host, "s3.example.com");
    }

    #[test]
    fn host_with_port_and_path() {
        let t = S3Transport::new(
            "http://127.0.0.1:9000/prefix",
            "bucket",
            "us-east-1",
            "AKIA",
            "secret",
        );
        assert_eq!(t.host, "127.0.0.1:9000");
        assert_eq!(
            t.url,
            "http://127.0.0.1:9000/prefix/bucket/clipboard/current.json"
        );
    }
}
