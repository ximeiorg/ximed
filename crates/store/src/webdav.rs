use async_trait::async_trait;
use domain::storage::{Storage, StorageError, Result};

/// WebDAV 存储后端（feature = webdav 启用）。
///
/// 语义映射（RFC 4918）：
/// - `put`    → `PUT {base}/{key}`（自动建父目录）
/// - `get`    → `GET {base}/{key}`
/// - `delete` → `DELETE {base}/{key}`（404 视为不存在，幂等）
/// - `list`   → `PROPFIND {base}/{prefix} Depth:1`，解析 200/207 响应中的资源 URL
///
/// 认证：Basic Auth（username/password 或 password_env）
pub struct WebDavStorage {
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    client: reqwest::Client,
}

impl WebDavStorage {
    pub fn new(
        base_url: String,
        username: Option<String>,
        password: Option<String>,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            username,
            password,
            client: reqwest::Client::new(),
        }
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match (&self.username, &self.password) {
            (Some(u), Some(p)) => req.basic_auth(u, Some(p)),
            _ => req,
        }
    }

    fn url(&self, key: &str) -> String {
        format!("{}/{}", self.base_url, key)
    }

    fn parent_dir(&self, key: &str) -> String {
        match key.rfind('/') {
            Some(i) => key[..i].to_string(),
            None => String::new(),
        }
    }

    /// 递归创建父目录（MKCOL，409/405 表示已存在则忽略）。
    async fn ensure_parents(&self, key: &str) -> Result<()> {
        let mut dirs: Vec<String> = Vec::new();
        let mut dir = self.parent_dir(key);
        while !dir.is_empty() {
            dirs.push(dir.clone());
            match dir.rfind('/') {
                Some(i) => dir = dir[..i].to_string(),
                None => break,
            }
        }
        for d in dirs.into_iter().rev() {
            let url = self.url(&d);
            let resp = self
                .auth(self.client.request(reqwest::Method::from_bytes(b"MKCOL").unwrap(), &url))
                .send()
                .await
                .map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
            // 409 = 已存在，405 = 父目录不存在（递归已保证）；两者均视为可继续
            if !resp.status().is_success() && resp.status() != reqwest::StatusCode::CONFLICT && resp.status() != reqwest::StatusCode::METHOD_NOT_ALLOWED {
                return Err(StorageError::Io(std::io::Error::other(format!(
                    "MKCOL {url} failed: {}",
                    resp.status()
                ))));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for WebDavStorage {
    async fn put(&self, key: &str, data: &[u8]) -> Result<()> {
        domain::storage::validate_key(key)?;
        self.ensure_parents(key).await?;
        let resp = self
            .auth(self.client.put(self.url(key)))
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(StorageError::Io(std::io::Error::other(format!("PUT failed: {}", resp.status()))))
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        domain::storage::validate_key(key)?;
        let resp = self
            .auth(self.client.get(self.url(key)))
            .send()
            .await
            .map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if resp.status().is_success() {
            let bytes = resp.bytes().await.map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
            Ok(Some(bytes.to_vec()))
        } else {
            Err(StorageError::Io(std::io::Error::other(format!("GET failed: {}", resp.status()))))
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        domain::storage::validate_key(key)?;
        let resp = self
            .auth(self.client.delete(self.url(key)))
            .send()
            .await
            .map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
        // 404 = 不存在，幂等成功
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(StorageError::Io(std::io::Error::other(format!("DELETE failed: {}", resp.status()))))
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let prefix = prefix.trim_end_matches('/');
        let url = if prefix.is_empty() {
            self.base_url.clone()
        } else {
            self.url(prefix)
        };
        let resp = self
            .auth(self.client.request(reqwest::Method::from_bytes(b"PROPFIND").unwrap(), &url).header("Depth", "1"))
            .send()
            .await
            .map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(Vec::new());
        }
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::MULTI_STATUS {
            let body = resp.text().await.map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
            // 从 <d:href> 提取资源路径，过滤目录（以 / 结尾），还原为 key
            let mut out = Vec::new();
            for cap in body.match_indices("<d:href>") {
                let rest = &body[cap.0 + cap.1.len()..];
                if let Some(end) = rest.find("</d:href>") {
                    let href = &rest[..end];
                    let decoded = urlencoding_decode(href);
                    if decoded.ends_with('/') {
                        continue; // 目录
                    }
                    // 去掉 base 前缀，得到 key
                    if let Some(key) = decoded.strip_prefix(&self.base_url) {
                        out.push(key.trim_start_matches('/').to_string());
                    }
                }
            }
            out.sort();
            Ok(out)
        } else {
            Err(StorageError::Io(std::io::Error::other(format!("PROPFIND failed: {}", resp.status()))))
        }
    }
}

/// 简单 URL 解码（%XX），无需引入 url 依赖。
fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("00");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_decode_handles_percent() {
        assert_eq!(urlencoding_decode("clipboard%2Fcurrent.json"), "clipboard/current.json");
        assert_eq!(urlencoding_decode("a%20b.txt"), "a b.txt");
        assert_eq!(urlencoding_decode("plain"), "plain");
        assert_eq!(urlencoding_decode("100%25"), "100%");
        // 无效 %XX 保持原样
        assert_eq!(urlencoding_decode("bad%zz"), "bad%zz");
    }

    #[test]
    fn url_mapping() {
        let s = WebDavStorage::new(
            "https://dav.example.com/xime/".to_string(),
            Some("u".to_string()),
            Some("p".to_string()),
        );
        // base_url 去掉尾斜杠
        assert_eq!(
            s.url("clipboard/current.json"),
            "https://dav.example.com/xime/clipboard/current.json"
        );
        assert_eq!(s.parent_dir("clipboard/current.json"), "clipboard");
        assert_eq!(s.parent_dir("a/b/c.json"), "a/b");
        assert_eq!(s.parent_dir("root.json"), "");
    }
}

