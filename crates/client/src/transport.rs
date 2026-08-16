//! 传输层：统一的服务器通道抽象。
//!
//! [`WsTransport`] 走 WS 实时通道（Basic Auth + clipboard.set/get + 广播推送）；
//! [`HttpTransport`] 走 HTTP 轮询兜底（PUT 上传 / GET 拉取 + If-None-Match）。
//! 引擎只依赖 [`Transport`] trait，便于测试 mock。

use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use xime_sync_domain::profile::Profile;
use xime_sync_domain::protocol::{ClientMessage, ServerMessage};

use crate::config::ClientConfig;

/// 传输层错误。
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("connect failed: {0}")]
    Connect(String),
    #[error("send failed: {0}")]
    Send(String),
    #[error("receive failed: {0}")]
    Recv(String),
    #[error("auth rejected")]
    AuthRejected,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// 统一传输通道。引擎持有 `Arc<dyn Transport>`。
#[async_trait]
pub trait Transport: Send + Sync {
    /// 推送本地 profile 到服务器（clipboard.set / HTTP PUT）。
    async fn push(&self, profile: &Profile) -> Result<(), TransportError>;
    /// 拉取服务器当前 profile（服务器为空返回 Ok(None)）。
    async fn pull(&self) -> Result<Option<Profile>, TransportError>;
    /// 阻塞等待下一次剪贴板变更事件（超时返回 Ok(None)，内部自处理心跳/轮询）。
    async fn next_event(&self, timeout: Duration) -> Result<Option<Profile>, TransportError>;
}

/// 构建 Basic Auth 头值。
pub fn basic_auth_header(username: &str, password: &str) -> String {
    use base64::Engine;
    let cred = format!("{username}:{password}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(cred.as_bytes())
    )
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 构造 WS 握手请求：由 URL 生成标准请求（含 Sec-WebSocket-Key / Upgrade / Version），
/// 再补 Basic Auth 认证头。
///
/// 不能手动用 `http::Request::builder()` 拼：tungstenite 的 `IntoClientRequest`
/// 对裸 `http::Request` 要求预置 `Sec-WebSocket-Key`，缺失会直接报
/// `MissingSecWebSocketKey` 导致握手失败。
fn build_connect_request(url: &str, auth: &str) -> Result<http::Request<()>, TransportError> {
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
            .map_err(|e| TransportError::Connect(e.to_string()))?;
    let header = auth
        .parse::<http::header::HeaderValue>()
        .map_err(|e| TransportError::Connect(e.to_string()))?;
    request
        .headers_mut()
        .insert(http::header::AUTHORIZATION, header);
    Ok(request)
}

/// WS 实时通道。连接拆分为 sink/stream 分别持锁，避免锁跨 await。
pub struct WsTransport {
    url: String,
    auth: String,
    sink: tokio::sync::Mutex<
        Option<futures_util::stream::SplitSink<WsStream, tokio_tungstenite::tungstenite::Message>>,
    >,
    stream: tokio::sync::Mutex<Option<futures_util::stream::SplitStream<WsStream>>>,
}

impl WsTransport {
    pub fn new(cfg: &ClientConfig, password: &str) -> Self {
        Self {
            url: cfg.ws_url(),
            auth: basic_auth_header(&cfg.username, password),
            sink: tokio::sync::Mutex::new(None),
            stream: tokio::sync::Mutex::new(None),
        }
    }

    async fn ensure_connected(&self) -> Result<(), TransportError> {
        if self.sink.lock().await.is_some() {
            return Ok(());
        }
        let request = build_connect_request(&self.url, &self.auth)?;
        let (ws, resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| TransportError::Connect(e.to_string()))?;
        if resp.status() != http::StatusCode::SWITCHING_PROTOCOLS {
            return Err(TransportError::Connect(format!(
                "handshake {}",
                resp.status()
            )));
        }
        let (sink, stream) = ws.split();
        *self.sink.lock().await = Some(sink);
        *self.stream.lock().await = Some(stream);
        Ok(())
    }

    /// 发送一条客户端消息。
    async fn send(&self, msg: &ClientMessage) -> Result<(), TransportError> {
        self.ensure_connected().await?;
        let text = serde_json::to_string(msg).map_err(|e| TransportError::Send(e.to_string()))?;
        let mut guard = self.sink.lock().await;
        let sink = guard
            .as_mut()
            .ok_or_else(|| TransportError::Send("no connection".into()))?;
        sink.send(tokio_tungstenite::tungstenite::Message::Text(text))
            .await
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    /// 阻塞读取下一条服务器消息（自动回 pong），连接断开返回 Err。
    async fn recv_raw(&self) -> Result<ServerMessage, TransportError> {
        self.ensure_connected().await?;
        loop {
            let mut guard = self.stream.lock().await;
            let stream = guard
                .as_mut()
                .ok_or_else(|| TransportError::Recv("no connection".into()))?;
            let frame = stream
                .next()
                .await
                .ok_or_else(|| TransportError::Recv("connection closed".into()))?
                .map_err(|e| TransportError::Recv(e.to_string()))?;
            drop(guard);
            match frame {
                tokio_tungstenite::tungstenite::Message::Text(text) => {
                    let msg: ServerMessage = serde_json::from_str(&text)
                        .map_err(|e| TransportError::Recv(format!("parse: {e}")))?;
                    return Ok(msg);
                }
                tokio_tungstenite::tungstenite::Message::Ping(payload) => {
                    self.send_pong(payload).await?;
                    continue;
                }
                _ => continue,
            }
        }
    }

    async fn send_pong(&self, payload: Vec<u8>) -> Result<(), TransportError> {
        let mut guard = self.sink.lock().await;
        let sink = guard
            .as_mut()
            .ok_or_else(|| TransportError::Send("no connection".into()))?;
        sink.send(tokio_tungstenite::tungstenite::Message::Pong(payload))
            .await
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    /// 关闭连接（供引擎重连）。
    pub fn reset(&self) {
        // tokio Mutex 不可在非 async 上下文安全锁，改为尝试（失败则忽略）
        if let Ok(mut s) = self.sink.try_lock() {
            *s = None;
        }
        if let Ok(mut s) = self.stream.try_lock() {
            *s = None;
        }
    }
}

/// HTTP 轮询通道（兜底）。
pub struct HttpTransport {
    url: String,
    auth: String,
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new(cfg: &ClientConfig, password: &str) -> Self {
        Self {
            url: cfg.http_api(),
            auth: basic_auth_header(&cfg.username, password),
            client: reqwest::Client::new(),
        }
    }

    /// 拉取剪贴板历史（GET /api/clipboard/history?limit=）。
    /// 服务器未启用 history 或失败时返回空列表（不视为错误）。
    pub async fn fetch_history(
        &self,
        limit: usize,
    ) -> Result<crate::history::HistoryResponse, TransportError> {
        let resp = self
            .client
            .get(format!("{}/history?limit={limit}", self.url))
            .header(http::header::AUTHORIZATION, &self.auth)
            .send()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        if resp.status() == http::StatusCode::UNAUTHORIZED {
            return Err(TransportError::AuthRejected);
        }
        if !resp.status().is_success() {
            return Err(TransportError::Recv(format!(
                "history GET {}",
                resp.status()
            )));
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        serde_json::from_slice(&body).map_err(|e| TransportError::Recv(e.to_string()))
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
        let resp = self
            .client
            .put(&self.url)
            .header(http::header::AUTHORIZATION, &self.auth)
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(profile).map_err(|e| TransportError::Send(e.to_string()))?)
            .send()
            .await
            .map_err(|e| TransportError::Send(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(TransportError::Send(format!("PUT {}", resp.status())));
        }
        Ok(())
    }

    async fn pull(&self) -> Result<Option<Profile>, TransportError> {
        let resp = self
            .client
            .get(&self.url)
            .header(http::header::AUTHORIZATION, &self.auth)
            .send()
            .await
            .map_err(|e| TransportError::Recv(e.to_string()))?;
        if resp.status() == http::StatusCode::NOT_MODIFIED {
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
        // HTTP 无推送：睡眠一个轮询周期后拉取（引擎按 hash 去重）
        tokio::time::sleep(timeout).await;
        self.pull().await
    }
}

#[async_trait]
impl Transport for WsTransport {
    async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
        self.send(&ClientMessage::ClipboardSet {
            data: profile.clone(),
        })
        .await
    }

    async fn pull(&self) -> Result<Option<Profile>, TransportError> {
        self.send(&ClientMessage::ClipboardGet).await?;
        loop {
            let msg = self.recv_raw().await?;
            match msg {
                ServerMessage::ClipboardSnapshot { data }
                | ServerMessage::ClipboardChanged { data } => {
                    return Ok(Some(data));
                }
                _ => continue,
            }
        }
    }

    async fn next_event(&self, timeout: Duration) -> Result<Option<Profile>, TransportError> {
        let msg = tokio::time::timeout(timeout, self.recv_raw())
            .await
            .map_err(|_| TransportError::Recv("timeout".into()))??;
        match msg {
            ServerMessage::ClipboardChanged { data }
            | ServerMessage::ClipboardSnapshot { data } => Ok(Some(data)),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_auth_header_encodes() {
        assert_eq!(
            basic_auth_header("alice", "secret"),
            "Basic YWxpY2U6c2VjcmV0"
        );
    }

    #[test]
    fn ws_handshake_request_carries_required_headers() {
        // 回归：旧实现手动拼 http::Request，缺失 Sec-WebSocket-Key 导致握手失败
        let req = build_connect_request("ws://127.0.0.1:8443/sync", "Basic YWxpY2U6c2VjcmV0")
            .expect("request must build");
        assert!(
            req.headers().contains_key(http::header::SEC_WEBSOCKET_KEY),
            "handshake must carry Sec-WebSocket-Key"
        );
        assert_eq!(
            req.headers()
                .get(http::header::UPGRADE)
                .and_then(|v| v.to_str().ok()),
            Some("websocket")
        );
        assert_eq!(
            req.headers()
                .get(http::header::SEC_WEBSOCKET_VERSION)
                .and_then(|v| v.to_str().ok()),
            Some("13")
        );
        assert_eq!(
            req.headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Basic YWxpY2U6c2VjcmV0")
        );
    }
}
