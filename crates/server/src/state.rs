use std::sync::Arc;

use service::ClipboardContext;

use crate::config::{AuthConfig, ClipboardConfig, HistoryConfig, SseConfig};

/// 应用层共享状态（handler 层使用）：业务上下文 + 认证/剪贴板/SSE/历史配置。
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
    /// 历史配置（保留条数上限）。
    pub history_cfg: HistoryConfig,
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
            history_cfg: HistoryConfig::default(),
        }
    }

    /// 带历史配置的构造（server 装配时传入配置）。
    pub fn with_history_cfg(mut self, history_cfg: HistoryConfig) -> Self {
        self.history_cfg = history_cfg;
        self
    }
}

pub type SharedState = Arc<AppState>;
