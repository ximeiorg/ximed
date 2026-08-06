use std::sync::Arc;

use service::ClipboardContext;

use crate::config::{AuthConfig, ClipboardConfig, SseConfig};

/// 应用层共享状态（handler 层使用）：业务上下文 + 认证/剪贴板/SSE 配置。
#[derive(Clone)]
pub struct AppState {
    /// 剪贴板业务上下文（store + 缓存 + 广播）。
    pub clipboard: ClipboardContext,
    /// 认证配置。
    pub auth: AuthConfig,
    /// 剪贴板配置（帧大小、心跳、超时）。
    pub clipboard_cfg: ClipboardConfig,
    /// SSE 配置（心跳间隔等）。
    pub sse_cfg: SseConfig,
}

impl AppState {
    pub fn new(
        clipboard: ClipboardContext,
        auth: AuthConfig,
        clipboard_cfg: ClipboardConfig,
        sse_cfg: SseConfig,
    ) -> Self {
        Self {
            clipboard,
            auth,
            clipboard_cfg,
            sse_cfg,
        }
    }
}

pub type SharedState = Arc<AppState>;
