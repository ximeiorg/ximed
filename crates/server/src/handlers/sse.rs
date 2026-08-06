use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use domain::protocol::{ClipboardEvent, sse_event};
use service::ClipboardContext;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::check_basic_auth;
use crate::state::SharedState;

/// SSE 端点处理器：`GET /events` → `text/event-stream`。
///
/// - 认证：Basic Auth（与 WS/HTTP 共用校验逻辑）
/// - 客户端带 `Last-Event-ID` 时，从该序号之后的事件补发（来自服务端事件缓冲）
/// - 之后实时订阅广播，收到变更推送 `event: clipboard` + `id: <seq>`
/// - 心跳：每 `heartbeat_ms` 发 `event: ping`
pub async fn sse_handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_basic_auth(&headers, &state.auth) {
        return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    // 解析 Last-Event-ID（无则从 0 开始，即不补发历史，仅实时）
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    let heartbeat = Duration::from_millis(state.sse_cfg.heartbeat_ms);
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(128);
    let ctx = state.clipboard.clone();

    tokio::spawn(async move {
        run_sse_stream(ctx, tx, last_event_id, heartbeat).await;
    });

    Sse::new(ReceiverStream::new(rx))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keep-alive"),
        )
        .into_response()
}

/// SSE 推送循环：先补发历史（> last_id），再订阅实时广播，同时发心跳。
async fn run_sse_stream(
    ctx: ClipboardContext,
    tx: mpsc::Sender<Result<Event, Infallible>>,
    last_event_id: u64,
    heartbeat: Duration,
) {
    // 1. 补发历史中序号 > last_event_id 的事件
    let mut max_sent = last_event_id;
    {
        let history = ctx.history.read().await;
        for ev in history.iter() {
            if ev.seq > last_event_id {
                if send_clipboard_event(&tx, ev).await.is_err() {
                    return;
                }
                max_sent = ev.seq;
            }
        }
    }

    // 2. 订阅实时广播
    let mut broadcast_rx = ctx.broadcast.subscribe();
    let mut heartbeat_interval = tokio::time::interval(heartbeat);
    heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            // 实时变更
            event = broadcast_rx.recv() => {
                match event {
                    Ok(ev) => {
                        // 补发阶段可能已覆盖该序号，去重
                        if ev.seq <= max_sent {
                            continue;
                        }
                        max_sent = ev.seq;
                        if send_clipboard_event(&tx, &ev).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => return, // 广播源关闭
                }
            }
            // 心跳保活
            _ = heartbeat_interval.tick() => {
                let ping = Event::default().event(sse_event::PING).data("{}");
                if tx.send(Ok(ping)).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// 发送一个剪贴板事件帧：`event: clipboard` + `id: <seq>` + `data: <profile json>`。
async fn send_clipboard_event(
    tx: &mpsc::Sender<Result<Event, Infallible>>,
    ev: &ClipboardEvent,
) -> Result<(), ()> {
    let json = serde_json::to_string(&ev.profile).unwrap_or_default();
    let event = Event::default()
        .event(sse_event::CLIPBOARD)
        .id(ev.seq.to_string())
        .data(json);
    tx.send(Ok(event)).await.map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    use crate::config::{AuthConfig, ClipboardConfig, SseConfig};
    use crate::state::AppState;
    use domain::profile::Profile;
    use domain::storage::Storage;
    use service::{ClipboardContext, ClipboardService};
    use store::local::LocalStorage;

    async fn test_state() -> Arc<AppState> {
        let store: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tempfile::tempdir().unwrap().path()));
        let auth = AuthConfig {
            username: "alice".to_string(),
            password: None,
            password_env: "XIMED_AUTH_PASSWORD".to_string(),
            max_connections: 32,
        };
        let clipboard_cfg = ClipboardConfig {
            max_frame_size: 1024 * 1024,
            idle_timeout_secs: 60,
            heartbeat_interval_secs: 30,
            default_profile: None,
        };
        let sse_cfg = SseConfig {
            enabled: true,
            heartbeat_ms: 60_000, // 测试期避免心跳干扰
        };
        Arc::new(AppState::new(
            ClipboardContext::new(store),
            auth,
            clipboard_cfg,
            sse_cfg,
        ))
    }

    fn auth_header() -> String {
        let encoded = base64::engine::general_purpose::STANDARD.encode("alice:s3cret");
        format!("Basic {encoded}")
    }

    async fn spawn_server(state: Arc<AppState>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route("/events", axum::routing::get(sse_handler))
            .with_state(state);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/events")
    }

    /// 发送一次 HTTP GET 请求，读取响应行 + 指定毫秒内的数据块（SSE 不会自动结束）。
    async fn http_get(url: &str, extra_headers: &[(&str, &str)]) -> (String, String) {
        let host = url.trim_start_matches("http://").split('/').next().unwrap();
        let path = url.trim_start_matches("http://").trim_start_matches(host);
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        let mut req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: {}\r\n",
            auth_header()
        );
        for (k, v) in extra_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();

        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(Duration::from_secs(3), reader.read_line(&mut line))
                .await
                .expect("timeout reading headers")
                .unwrap();
            if n == 0 || line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        // 读取一小段时间内的数据（等待服务端补发/推送后主动断开）
        let mut body = String::new();
        let mut buf = [0u8; 2048];
        for _ in 0..20 {
            match tokio::time::timeout(Duration::from_millis(100), reader.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => body.push_str(&String::from_utf8_lossy(&buf[..n])),
                _ => break,
            }
        }
        (headers, body)
    }

    #[tokio::test]
    async fn sse_auth_required() {
        let state = test_state().await;
        let url = spawn_server(state).await;
        let host = url.trim_start_matches("http://").split('/').next().unwrap();
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        stream
            .write_all(format!("GET /events HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        let _ = reader.read_line(&mut status).await;
        assert!(
            status.starts_with("HTTP/1.1 401"),
            "unauthenticated SSE must 401, got {status}"
        );
    }

    #[tokio::test]
    async fn sse_bad_password_rejected() {
        let state = test_state().await;
        let url = spawn_server(state).await;
        let host = url.trim_start_matches("http://").split('/').next().unwrap();
        let bad = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("alice:wrong")
        );
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        stream
            .write_all(
                format!("GET /events HTTP/1.1\r\nHost: {host}\r\nAuthorization: {bad}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        let _ = reader.read_line(&mut status).await;
        assert!(
            status.starts_with("HTTP/1.1 401"),
            "bad password must 401, got {status}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // env_lock 需覆盖整个测试生命周期，防止 env 读写竞争
    async fn sse_push_event_and_last_event_id() {
        let _g = crate::config::env_lock();
        unsafe {
            std::env::set_var("XIMED_AUTH_PASSWORD", "s3cret");
        }
        let state = test_state().await;
        let url = spawn_server(state.clone()).await;

        // 先写入两个事件，序号 1、2
        ClipboardService
            .put(
                &state.clipboard,
                Profile::from_text("event-one", Some("sse-a".to_string())),
            )
            .await
            .unwrap();
        ClipboardService
            .put(
                &state.clipboard,
                Profile::from_text("event-two", Some("sse-b".to_string())),
            )
            .await
            .unwrap();

        // 带 Last-Event-ID: 1 → 应补发 seq=2（event-two）
        let (headers, body) = http_get(&url, &[("Last-Event-ID", "1")]).await;
        assert!(
            headers.starts_with("HTTP/1.1 200"),
            "SSE must 200, got {headers}"
        );
        assert!(
            headers.contains("text/event-stream"),
            "must be event-stream"
        );
        assert!(body.contains("id: 2"), "must replay seq 2, body: {body}");
        assert!(
            body.contains("event-two"),
            "must contain event-two, body: {body}"
        );
        assert!(!body.contains("event-one"), "must not replay seq 1");

        // Last-Event-ID: 0 → 补发 seq 1、2
        let (_h, body) = http_get(&url, &[("Last-Event-ID", "0")]).await;
        assert!(
            body.contains("id: 1") && body.contains("id: 2"),
            "must replay 1 and 2, body: {body}"
        );
    }
}
