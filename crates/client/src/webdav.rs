//! WebDAV 通道：当前剪贴板 = 网盘上的一个 JSON 文件。
//!
//! 语义与 HTTP 通道完全一致（单值资源、最后写入胜出），只是载体换成用户自己的
//! 网盘，不需要任何自建服务：
//!
//! - `push` = `PUT <url>/<file>`，body 是 `Profile` JSON
//! - `pull` = `GET`，200 解析，404 / 204 / 304 都视为「远端没有新内容」
//! - `next_event` = 睡眠一个轮询周期再 `pull`（WebDAV 没有推送能力）
//!
//! 不用 `If-Match` 做条件写：剪贴板是单值资源，最后写入胜出就是协议语义
//! （协议文档 §8.3）。加了条件写反而会在两端同时复制时把一方判成失败，用户看到
//! 的是「同步报错但内容其实对」。
//!
//! 去重不在这里做：引擎按 `hash` 比对（`SyncEngine::apply_remote`），传输层只管
//! 把字节送到/取回。

use std::time::Duration;

use async_trait::async_trait;
use xime_sync_domain::profile::Profile;

use crate::config::WebDavSettings;
use crate::transport::{Transport, TransportError, basic_auth_header};

/// WebDAV 通道。
pub struct WebDavTransport {
    client: reqwest::Client,
    /// 目标文件完整地址。
    endpoint: String,
    /// 目录地址（PROPFIND 用）。
    dir: String,
    /// Basic Auth 头值；未配置用户名时为 `None`（少数网盘允许匿名读写）。
    auth: Option<String>,
}

impl WebDavTransport {
    /// 由配置构建。密码取环境变量优先（见 [`WebDavSettings::password`]）。
    pub fn new(cfg: &WebDavSettings) -> Self {
        let auth = if cfg.username.trim().is_empty() {
            None
        } else {
            let password = cfg.password().unwrap_or_default();
            Some(basic_auth_header(cfg.username.trim(), &password))
        };
        Self {
            client: reqwest::Client::new(),
            endpoint: cfg.endpoint(),
            dir: cfg.dir_endpoint(),
            auth,
        }
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self.client.request(method, url);
        match &self.auth {
            Some(auth) => builder.header(http::header::AUTHORIZATION, auth),
            None => builder,
        }
    }

    /// 测试连接：对目录做一次 `PROPFIND`（Depth: 0）。
    ///
    /// 打在目录而不是文件上：首次配置时文件还不存在，打文件会得到 404，用户会
    /// 误以为账号有问题；目录存在即说明地址、认证、写入权限都通了。
    pub async fn test_connection(&self) -> Result<(), TransportError> {
        if self.dir.is_empty() {
            return Err(TransportError::Connect("尚未填写 WebDAV 地址".into()));
        }
        let resp = self
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").expect("静态方法"),
                &self.dir,
            )
            .header("Depth", "0")
            .send()
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        match resp.status().as_u16() {
            401 | 403 => Err(TransportError::AuthRejected),
            404 => Err(TransportError::Connect(
                "目录不存在，请检查地址（多数网盘要求在地址里带上应用目录）".into(),
            )),
            // 207 是标准的 Multi-Status；个别网盘对 PROPFIND 回 200。
            code if (200..300).contains(&code) || code == 207 => Ok(()),
            code => Err(TransportError::Connect(format!("PROPFIND {code}"))),
        }
    }
}

#[async_trait]
impl Transport for WebDavTransport {
    async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
        let body = serde_json::to_vec(profile).map_err(|e| TransportError::Send(e.to_string()))?;
        let resp = self
            .request(reqwest::Method::PUT, &self.endpoint)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| TransportError::Send(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(TransportError::AuthRejected);
        }
        if !resp.status().is_success() {
            return Err(TransportError::Send(format!("PUT {}", resp.status())));
        }
        Ok(())
    }

    async fn pull(&self) -> Result<Option<Profile>, TransportError> {
        let resp = self
            .request(reqwest::Method::GET, &self.endpoint)
            .send()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        match resp.status() {
            // 还没有远端内容：不是错误，首台设备第一次同步就是这个状态。
            reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::NO_CONTENT => return Ok(None),
            reqwest::StatusCode::NOT_MODIFIED => return Ok(None),
            reqwest::StatusCode::UNAUTHORIZED => return Err(TransportError::AuthRejected),
            s if !s.is_success() => return Err(TransportError::Recv(format!("GET {s}"))),
            _ => {}
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        if body.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|e| TransportError::Recv(e.to_string()))
    }

    async fn next_event(&self, timeout: Duration) -> Result<Option<Profile>, TransportError> {
        // 网盘没有推送通道：退化成轮询，引擎按 hash 去重。
        tokio::time::sleep(timeout).await;
        self.pull().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// 极简 WebDAV mock：只实现 PUT / GET / PROPFIND 三个方法。
    ///
    /// 不值得为三个方法引入测试框架依赖；请求行与 `Content-Length` 就够解析，
    /// 而且这样测的是真实 HTTP 语义（状态码、头部），不是伪实现。
    struct MockDav {
        content: Arc<Mutex<Option<Vec<u8>>>>,
        require_auth: Option<String>,
        /// 记录收到的 (方法, 路径)，用于断言请求打到了正确地址。
        seen: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl MockDav {
        fn new() -> Self {
            Self {
                content: Arc::new(Mutex::new(None)),
                require_auth: None,
                seen: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_auth(mut self, header: impl Into<String>) -> Self {
            self.require_auth = Some(header.into());
            self
        }

        fn store(&self, bytes: &[u8]) {
            *self.content.lock().unwrap() = Some(bytes.to_vec());
        }

        fn spawn(self: &Arc<Self>) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let shared = Arc::clone(self);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    handle(stream, &shared);
                }
            });
            format!("http://{addr}/dav")
        }
    }

    fn handle(mut stream: TcpStream, dav: &MockDav) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        // 读到头部结束
        let head_end = loop {
            let Ok(n) = stream.read(&mut chunk) else {
                return;
            };
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();

        let mut content_length = 0usize;
        let mut authorized = dav.require_auth.is_none();
        for line in lines {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
            if lower.starts_with("authorization:")
                && dav
                    .require_auth
                    .as_deref()
                    .is_some_and(|expected| line[14..].trim() == expected)
            {
                authorized = true;
            }
        }
        if let Ok(mut seen) = dav.seen.lock() {
            seen.push((method.clone(), path));
        }
        if !authorized {
            return respond(&mut stream, 401, b"");
        }

        // 读满 body
        while buf.len() < head_end + content_length {
            let Ok(n) = stream.read(&mut chunk) else {
                return;
            };
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = buf[head_end..(head_end + content_length).min(buf.len())].to_vec();

        match method.as_str() {
            "PUT" => {
                dav.store(&body);
                respond(&mut stream, 201, b"");
            }
            "GET" => {
                let content = dav.content.lock().unwrap().clone();
                match content {
                    Some(bytes) => respond(&mut stream, 200, &bytes),
                    None => respond(&mut stream, 404, b""),
                }
            }
            "PROPFIND" => respond(&mut stream, 207, b""),
            _ => respond(&mut stream, 405, b""),
        }
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn respond(stream: &mut TcpStream, code: u16, body: &[u8]) {
        let reason = match code {
            200 => "OK",
            201 => "Created",
            207 => "Multi-Status",
            401 => "Unauthorized",
            404 => "Not Found",
            _ => "Error",
        };
        let head = format!(
            "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    fn settings(url: &str) -> WebDavSettings {
        WebDavSettings {
            url: url.to_string(),
            file: "clipboard.json".to_string(),
            username: "alice".to_string(),
            password: Some("secret".to_string()),
            ..WebDavSettings::default()
        }
    }

    #[test]
    fn 地址拼接不产生双斜杠() {
        let mut cfg = WebDavSettings {
            url: "https://dav.example.com/dav/ximed/".to_string(),
            file: "/clipboard.json".to_string(),
            ..WebDavSettings::default()
        };
        assert_eq!(
            cfg.endpoint(),
            "https://dav.example.com/dav/ximed/clipboard.json"
        );
        assert_eq!(cfg.dir_endpoint(), "https://dav.example.com/dav/ximed");

        cfg.url = " https://dav.example.com/dav/ximed ".to_string();
        cfg.file = "sub/clip.json".to_string();
        assert_eq!(
            cfg.endpoint(),
            "https://dav.example.com/dav/ximed/sub/clip.json"
        );
    }

    #[test]
    fn 未配置地址时不算可用() {
        assert!(!WebDavSettings::default().is_configured());
        assert!(settings("http://x").is_configured());
    }

    #[tokio::test]
    async fn 推送后能原样拉取回来() {
        let dav = Arc::new(MockDav::new());
        let url = dav.spawn();
        let t = WebDavTransport::new(&settings(&url));

        // 远端为空：不是错误，首台设备第一次同步就是这个状态
        assert!(t.pull().await.unwrap().is_none(), "404 应视为暂无内容");

        let profile = Profile::from_text("hello dav", Some("dev-a".into()));
        t.push(&profile).await.unwrap();
        let back = t.pull().await.unwrap().expect("push 之后应能拉回");
        assert_eq!(back.text, profile.text);
        assert_eq!(back.hash, profile.hash);
    }

    #[tokio::test]
    async fn 认证失败报_auth_rejected() {
        let dav = Arc::new(MockDav::new().with_auth(basic_auth_header("alice", "other")));
        let url = dav.spawn();
        let t = WebDavTransport::new(&settings(&url));
        let err = t.pull().await.expect_err("密码不对必须报错");
        assert!(matches!(err, TransportError::AuthRejected), "got {err:?}");
        assert!(matches!(
            t.test_connection().await,
            Err(TransportError::AuthRejected)
        ));
    }

    #[tokio::test]
    async fn 测试连接打在目录上() {
        let dav = Arc::new(MockDav::new().with_auth(basic_auth_header("alice", "secret")));
        let url = dav.spawn();
        let t = WebDavTransport::new(&settings(&url));
        t.test_connection().await.expect("目录 PROPFIND 应成功");
        let seen = dav.seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![("PROPFIND".to_string(), "/dav".to_string())],
            "必须打目录、不能打文件（首次配置时文件还不存在）"
        );
    }

    #[tokio::test]
    async fn 空远端不算错误() {
        let dav = Arc::new(MockDav::new());
        dav.store(b"");
        let url = dav.spawn();
        let t = WebDavTransport::new(&settings(&url));
        assert!(t.pull().await.unwrap().is_none(), "空 body 视为没有内容");
    }
}
