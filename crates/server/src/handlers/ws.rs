use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use service::{ClipboardService, PutOutcome};
use domain::protocol::{ClientMessage, ServerMessage};

use crate::auth::check_basic_auth;
use crate::state::SharedState;

/// WS 端点处理器：握手时校验 Basic Auth，通过后升级为 WebSocket 长连接。
///
/// 协议（见设计文档 3.2）：
/// - 服务器首帧发 `auth.ok` 确认
/// - 客户端发 `clipboard.set` / `clipboard.get` / `ping`
/// - 服务器发 `clipboard.changed`（订阅广播）/ `clipboard.snapshot` / `ping`
/// - 空闲超时（idle_timeout_secs）无消息判定离线
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<SharedState>,
) -> Response {
    if !check_basic_auth(&headers, &state.auth) {
        return (axum::http::StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// 单连接处理：广播订阅 + 客户端消息循环（空闲超时断开）。
async fn handle_socket(socket: WebSocket, state: SharedState) {
    let (mut sender, mut receiver) = socket.split();
    let idle_timeout = Duration::from_secs(state.clipboard_cfg.idle_timeout_secs);

    // 首帧：认证确认（握手已通过 Basic Auth，此处直接 ack）
    let ack = ServerMessage::AuthOk {
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    if sender
        .send(Message::Text(serde_json::to_string(&ack).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

    // 订阅变更广播（广播源与 HTTP PUT / SSE 共享）
    let mut broadcast_rx = state.clipboard.broadcast.subscribe();
    // 附件上传状态：客户端先发 file.set 声明文件名，随后发 binary 数据帧
    let mut pending_file: Option<String> = None;

    loop {
        tokio::select! {
            // 分支 1：客户端消息，带空闲超时
            msg = tokio::time::timeout(idle_timeout, receiver.next()) => {
                match msg {
                    // 空闲超时：判定离线
                    Err(_) => {
                        let _ = sender.send(Message::Close(None)).await;
                        return;
                    }
                    Ok(None) => return, // 客户端断开
                    Ok(Some(Err(_))) => return,
                    Ok(Some(Ok(msg))) => {
                        if !handle_client_message(msg, &mut sender, &state, &mut pending_file).await {
                            return;
                        }
                    }
                }
            }
            // 分支 2：广播事件 → 推送给客户端
            broadcast = broadcast_rx.recv() => {
                match broadcast {
                    Ok(event) => {
                        let changed = ServerMessage::ClipboardChanged { data: event.profile };
                        if sender
                            .send(Message::Text(serde_json::to_string(&changed).unwrap().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => return, // 广播源关闭（服务停机）
                }
            }
        }
    }
}

/// 处理单条客户端消息，返回 false 表示应断开连接。
async fn handle_client_message(
    msg: Message,
    sender: &mut (impl SinkExt<Message> + Unpin),
    state: &SharedState,
    pending_file: &mut Option<String>,
) -> bool {
    match msg {
        Message::Text(text) => {
            let client_msg: ClientMessage = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => return true, // 忽略无法解析的帧
            };
            match client_msg {
                ClientMessage::Auth { .. } => {
                    // 握手已认证，重复 auth 帧直接 ack 即可
                    let ack = ServerMessage::AuthOk {
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    };
                    send_json(sender, &ack).await
                }
                ClientMessage::ClipboardSet { data } => {
                    // 幂等去重：service 层对相同 hash 不广播、不落盘
                    match ClipboardService.put(&state.clipboard, data).await {
                        Ok(PutOutcome::Saved(_)) => true,
                        Ok(PutOutcome::Unchanged) => true,
                        Err(_) => true,
                    }
                }
                ClientMessage::ClipboardGet => {
                    let profile = ClipboardService.load_current(&state.clipboard).await;
                    send_json(sender, &ServerMessage::ClipboardSnapshot { data: profile }).await
                }
                ClientMessage::FileSet { name } => {
                    // 声明附件名（路径穿越由 service 校验，非法则忽略）
                    if ClipboardService::file_key(&name).is_ok() {
                        *pending_file = Some(name);
                    } else {
                        *pending_file = None;
                    }
                    true
                }
                ClientMessage::FileGet { name } => {
                    // 回 binary 帧：存在则数据，不存在回空
                    let data = ClipboardService
                        .get_file(&state.clipboard, &name)
                        .await
                        .unwrap_or_default()
                        .unwrap_or_default();
                    sender.send(Message::Binary(data.into())).await.is_ok()
                }
                ClientMessage::Ping => true,
            }
        }
        Message::Binary(data) => {
            // 附件数据帧：落在 pending_file 声明过的文件名下
            if let Some(name) = pending_file.take() {
                if ClipboardService
                    .put_file(&state.clipboard, &name, &data)
                    .await
                    .is_err()
                {
                    return false; // 存储失败断开
                }
            }
            true
        }
        Message::Ping(_) => true, // 底层已自动回 pong
        Message::Pong(_) => true,
        Message::Close(_) => false,
    }
}

async fn send_json<S>(sender: &mut S, msg: &ServerMessage) -> bool
where
    S: SinkExt<Message> + Unpin,
{
    match serde_json::to_string(msg) {
        Ok(text) => sender.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use domain::profile::Profile;
    use domain::storage::Storage;

    use crate::config::{AuthConfig, ClipboardConfig, SseConfig};
    use crate::state::AppState;
    use service::ClipboardContext;
    use store::local::LocalStorage;

    async fn test_state() -> Arc<AppState> {
        let store: Arc<dyn Storage> = Arc::new(LocalStorage::new(tempfile::tempdir().unwrap().path()));
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
            heartbeat_ms: 15_000,
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
            .route("/sync", axum::routing::get(ws_handler))
            .with_state(state);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("ws://{addr}/sync")
    }

    type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    async fn connect(url: &str, auth: &str) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
        let mut request = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url).unwrap();
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            auth.parse().unwrap(),
        );
        let (ws, _) = tokio_tungstenite::connect_async(request).await?;
        Ok(ws)
    }

    #[tokio::test]
    async fn ws_auth_required() {
        let _g = crate::config::env_lock();
        unsafe { std::env::set_var("XIMED_AUTH_PASSWORD", "s3cret"); }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .route("/sync", axum::routing::get(ws_handler))
            .with_state(test_state().await);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let res = tokio_tungstenite::connect_async(format!("ws://{addr}/sync")).await;
        assert!(res.is_err(), "unauthenticated ws upgrade must fail");
    }

    #[tokio::test]
    async fn ws_bad_password_rejected() {
        let _g = crate::config::env_lock();
        unsafe { std::env::set_var("XIMED_AUTH_PASSWORD", "s3cret"); }
        let state = test_state().await;
        let url = spawn_server(state).await;
        let bad = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("alice:wrong"));
        let res = connect(&url, &bad).await;
        assert!(res.is_err(), "wrong password must fail the ws handshake");
    }

    #[tokio::test]
    async fn ws_auth_ok_and_broadcast() {
        let _g = crate::config::env_lock();
        unsafe { std::env::set_var("XIMED_AUTH_PASSWORD", "s3cret"); }
        let state = test_state().await;
        let url = spawn_server(state.clone()).await;
        let auth = auth_header();

        let mut ws_a = connect(&url, &auth).await.unwrap();
        let mut ws_b = connect(&url, &auth).await.unwrap();

        let ack_a = ws_a.next().await.unwrap().unwrap();
        let ack_b = ws_b.next().await.unwrap().unwrap();
        assert!(ack_a.to_text().unwrap().contains("\"type\":\"auth.ok\""));
        assert!(ack_b.to_text().unwrap().contains("\"type\":\"auth.ok\""));

        // A 发 clipboard.get → snapshot
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        ws_a.send(WsMessage::Text(r#"{"type":"clipboard.get"}"#.into())).await.unwrap();
        let snap = ws_a.next().await.unwrap().unwrap();
        assert!(snap.to_text().unwrap().contains("\"type\":\"clipboard.snapshot\""));

        // A 发 clipboard.set → B 收到 clipboard.changed
        let profile = Profile::from_text("ws广播测试", Some("ws-a".to_string()));
        let set_msg = serde_json::json!({ "type": "clipboard.set", "data": profile });
        ws_a.send(WsMessage::Text(set_msg.to_string().into())).await.unwrap();

        let changed = ws_b.next().await.unwrap().unwrap();
        let text = changed.to_text().unwrap();
        assert!(text.contains("\"type\":\"clipboard.changed\""));
        assert!(text.contains("ws广播测试"));
        assert!(text.contains("\"source\":\"ws-a\""));

        // A 也应收到第一次 set 的 changed（订阅了广播）
        let changed_a = ws_a.next().await.unwrap().unwrap();
        assert!(changed_a.to_text().unwrap().contains("ws广播测试"));

        // B 幂等：再次 set 相同 hash，两端都不应再收到 changed（服务端不广播）
        ws_b.send(WsMessage::Text(set_msg.to_string().into())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let extra_a = tokio::time::timeout(Duration::from_millis(300), ws_a.next()).await;
        let extra_b = tokio::time::timeout(Duration::from_millis(300), ws_b.next()).await;
        assert!(extra_a.is_err(), "idempotent set must not rebroadcast (a)");
        assert!(extra_b.is_err(), "idempotent set must not rebroadcast (b)");
    }
}
