//! 直连适配器端到端测试：本地 TCP 服务模拟 WebDAV/S3 端点，
//! 验证 push/pull/next_event 的真实 HTTP 往返（不含 server，纯 client 直连）。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use client::direct::{S3Transport, WebDavTransport};
use client::transport::Transport;
use domain::profile::Profile;

/// 极简 HTTP 服务：监听随机端口，按请求路径/方法返回预设行为。
/// - PUT `/*` → 200，记录 body 到共享状态（按 URL 键存储）
/// - GET `/*` → 200 返回存储的 body，未存储返回 404
struct MockServer {
    state: Arc<Mutex<Vec<u8>>>,
    addr: String,
}

impl MockServer {
    fn spawn() -> Self {
        let state = Arc::new(Mutex::new(Vec::<u8>::new()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let s2 = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let state = s2.clone();
                std::thread::spawn(move || handle(stream, &state));
            }
        });
        Self { state, addr }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn stored(&self) -> Vec<u8> {
        self.state.lock().unwrap().clone()
    }
}

fn handle(mut stream: TcpStream, state: &Arc<Mutex<Vec<u8>>>) {
    // 循环读取直到拿到完整请求（CRLFCRLF 结束的头部）
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let req = String::from_utf8_lossy(&buf).to_string();
    let mut lines = req.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    if method == "PUT" {
        // 从 body 段提取内容（简化：以空行后的内容为准）
        let body_start = req.find("\r\n\r\n").map(|i| i + 4).unwrap_or(buf.len());
        let body = buf[body_start..].to_vec();
        *state.lock().unwrap() = body;
        let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
    } else if method == "GET" {
        let stored = state.lock().unwrap().clone();
        if stored.is_empty() {
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
        } else {
            let len = stored.len();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {len}\r\n\r\n"
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&stored);
        }
    } else {
        let resp = "HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
    }
}

fn sample_profile() -> Profile {
    Profile::from_text("直连同步测试", Some("dev-a".to_string()))
}

#[tokio::test]
async fn webdav_direct_push_pull_roundtrip() {
    let srv = MockServer::spawn();
    let t = WebDavTransport::new(&srv.url("/dav"), "user", "pass");
    let profile = sample_profile();

    // push → 远端存储该 profile
    t.push(&profile).await.expect("push must succeed");
    let stored = srv.stored();
    let roundtrip: Profile = serde_json::from_slice(&stored).expect("stored profile must parse");
    assert_eq!(roundtrip, profile);

    // pull → 拉回相同 profile
    let pulled = t
        .pull()
        .await
        .expect("pull must succeed")
        .expect("must be Some");
    assert_eq!(pulled, profile);
}

#[tokio::test]
async fn webdav_direct_pull_empty_returns_none() {
    let srv = MockServer::spawn();
    let t = WebDavTransport::new(&srv.url("/dav"), "user", "pass");
    assert_eq!(
        t.pull().await.unwrap(),
        None,
        "empty remote must yield None"
    );
}

#[tokio::test]
async fn s3_direct_push_pull_roundtrip() {
    let srv = MockServer::spawn();
    let t = S3Transport::new(&srv.url(""), "bucket", "us-east-1", "AKIAEXAMPLE", "secret");
    let profile = sample_profile();

    t.push(&profile).await.expect("push must succeed");
    let stored = srv.stored();
    let roundtrip: Profile = serde_json::from_slice(&stored).expect("stored profile must parse");
    assert_eq!(roundtrip, profile);

    let pulled = t
        .pull()
        .await
        .expect("pull must succeed")
        .expect("must be Some");
    assert_eq!(pulled, profile);
}

#[tokio::test]
async fn s3_direct_request_carries_auth_headers() {
    // 扩展 MockServer 记录请求头比较麻烦，这里用独立实现：
    // 验证签名头存在（Authorization / x-amz-date / x-amz-content-sha256）。
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let headers = Arc::new(Mutex::new(String::new()));
    let h = headers.clone();
    std::thread::spawn(move || {
        if let Ok(mut stream) = listener.accept().map(|(s, _)| s) {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            *h.lock().unwrap() = String::from_utf8_lossy(&buf).to_string();
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    let t = S3Transport::new(
        &format!("http://{addr}"),
        "bucket",
        "us-east-1",
        "AKIAEXAMPLE",
        "secret",
    );
    // pull 触发一次 GET 请求，随后检查捕获的请求头
    let _ = t.pull().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let req = headers.lock().unwrap().clone();
    assert!(
        req.to_lowercase().contains("x-amz-date:"),
        "missing x-amz-date: {req}"
    );
    assert!(
        req.to_lowercase().contains("x-amz-content-sha256:"),
        "missing x-amz-content-sha256: {req}"
    );
    assert!(
        req.contains("AWS4-HMAC-SHA256"),
        "missing SigV4 Authorization: {req}"
    );
}
