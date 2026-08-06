use serde::{Deserialize, Serialize};

use crate::profile::Profile;

/// WS 通道客户端 → 服务器消息帧（JSON 文本帧，`type` 字段分发）。
///
/// ```json
/// { "type": "auth", "user": "alice", "token": "xxx" }
/// { "type": "clipboard.set", "data": { ...profile... } }
/// { "type": "clipboard.get" }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// 首帧认证（与 HTTP 头 Basic Auth 等效）。
    Auth { user: String, token: String },
    #[serde(rename = "clipboard.set")]
    ClipboardSet { data: Profile },
    #[serde(rename = "clipboard.get")]
    ClipboardGet,
    /// 心跳。
    Ping,
}

/// WS 通道服务器 → 客户端消息帧。
///
/// ```json
/// { "type": "auth.ok", "version": "0.1.0" }
/// { "type": "auth.error", "reason": "invalid credentials" }
/// { "type": "clipboard.changed", "data": { ...profile... } }
/// { "type": "clipboard.snapshot", "data": { ...profile... } }
/// { "type": "ping" }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    #[serde(rename = "auth.ok")]
    AuthOk { version: String },
    #[serde(rename = "auth.error")]
    AuthError { reason: String },
    #[serde(rename = "clipboard.changed")]
    ClipboardChanged { data: Profile },
    #[serde(rename = "clipboard.snapshot")]
    ClipboardSnapshot { data: Profile },
    Ping,
}

/// SSE 事件名常量。
pub mod sse_event {
    /// 剪贴板变更事件：`event: clipboard`，data 为 Profile JSON。
    pub const CLIPBOARD: &str = "clipboard";
    /// 心跳事件：`event: ping`。
    pub const PING: &str = "ping";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_auth_serializes() {
        let msg = ClientMessage::Auth {
            user: "alice".to_string(),
            token: "xxx".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"auth","user":"alice","token":"xxx"}"#);
    }

    #[test]
    fn client_message_clipboard_set_serializes() {
        let p = Profile::from_text("hello", Some("device-a".to_string()));
        let msg = ClientMessage::ClipboardSet { data: p };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.starts_with(r#"{"type":"clipboard.set","data":{"type":"text","hash":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824","text":"hello","has_data":false,"data_name":null,"size":5,"source":"device-a"}"#), "{json}");
    }

    #[test]
    fn client_message_deserialize() {
        let cases = [
            (r#"{"type":"clipboard.get"}"#, ClientMessage::ClipboardGet),
            (r#"{"type":"ping"}"#, ClientMessage::Ping),
            (
                r#"{"type":"auth","user":"alice","token":"xxx"}"#,
                ClientMessage::Auth { user: "alice".to_string(), token: "xxx".to_string() },
            ),
        ];
        for (json, expected) in cases {
            let msg: ClientMessage = serde_json::from_str(json).unwrap();
            assert_eq!(msg, expected);
        }
    }

    #[test]
    fn client_message_rejects_unknown_type() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"bogus"}"#);
        assert!(err.is_err());
    }

    #[test]
    fn server_message_serializes() {
        let p = Profile::from_text("hi", None);
        let changed = ServerMessage::ClipboardChanged { data: p.clone() };
        let changed_json = serde_json::to_string(&changed).unwrap();
        assert!(
            changed_json.starts_with(r#"{"type":"clipboard.changed","data":{"#),
            "actual: {changed_json}"
        );

        let snapshot = ServerMessage::ClipboardSnapshot { data: p };
        let snapshot_json = serde_json::to_string(&snapshot).unwrap();
        assert!(
            snapshot_json.starts_with(r#"{"type":"clipboard.snapshot","data":{"#),
            "actual: {snapshot_json}"
        );

        assert_eq!(
            serde_json::to_string(&ServerMessage::AuthOk { version: "0.1.0".to_string() }).unwrap(),
            r#"{"type":"auth.ok","version":"0.1.0"}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerMessage::AuthError { reason: "invalid credentials".to_string() }).unwrap(),
            r#"{"type":"auth.error","reason":"invalid credentials"}"#
        );
        assert_eq!(serde_json::to_string(&ServerMessage::Ping).unwrap(), r#"{"type":"ping"}"#);
    }

    #[test]
    fn server_message_deserialize() {
        let msg: ServerMessage = serde_json::from_str(r#"{"type":"clipboard.changed","data":{"type":"text","hash":"h","text":"t","has_data":false,"data_name":null,"size":1,"source":null}}"#).unwrap();
        match msg {
            ServerMessage::ClipboardChanged { data } => {
                assert_eq!(data.hash, "h");
                assert_eq!(data.text, "t");
            }
            other => panic!("expected changed, got {other:?}"),
        }
    }
}
