//! 同步通道：有序列表、健康窗口与切换决策。
//!
//! 三种内置通道（`http` / `webdav` / `p2p`）共用同一套选择逻辑：**列表有序、
//! 首个优先**。语义与 XimeYi / Android 的 `channels` 一致，三端可以互换配置。
//!
//! 为什么不是「多选并行」：剪贴板是单值资源，双写会让同一条内容被推两次，远端
//! 回写时还可能互相触发（协议文档 §2.4）。所以同一时刻只跑一条通道；「降级」靠
//! 列表顺序 + 一个 `current` 表达，而不是同时开两条。
//!
//! 为什么只有 P2P 参与健康判定：P2P 有明确的连接事件（`Event::Peer`），能区分
//! 「连着」和「没连」；而 HTTP / WebDAV 只有「本次 push 成功与否」，一次失败可能
//! 只是网络抖动，拿它做切换判据会横跳。因此其余通道按用户给的顺序一直用，直到
//! 用户改配置。

use std::time::{Duration, Instant};

/// 内置通道 id：ximed server（本机或远端）。
pub const CHANNEL_HTTP: &str = "http";
/// 内置通道 id：WebDAV 第三方网盘。
pub const CHANNEL_WEBDAV: &str = "webdav";
/// 内置通道 id：局域网直连。
pub const CHANNEL_P2P: &str = "p2p";

/// P2P 连上后稳定这么久才算「健康」，避免 Wi-Fi 抖动导致通道横跳。
pub const P2P_STABLE: Duration = Duration::from_secs(5);
/// P2P 掉线（或一直连不上）超过这么久才判定不可用。
pub const P2P_LOST_GRACE: Duration = Duration::from_secs(10);
/// 切到备用通道后的最短驻留时间：远程服务来回切会撞限流。
pub const MIN_DWELL: Duration = Duration::from_secs(30);

/// 一个通道。
///
/// 只认内置 id：配置里出现别的 id（旧版插件、手写错字）由
/// [`Channel::from_id`] 返回 `None`，调用方跳过并告警——宽松接受未知通道会让
/// 「写错 id」表现为「同步静默关闭」，比直接报错更难排查。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// 内置：ximed server（HTTP/WS）。
    Http,
    /// 内置：WebDAV 网盘。
    WebDav,
    /// 内置：局域网直连。
    P2p,
}

impl Channel {
    /// 由配置里的 id 解析；未知 id 返回 `None`。
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim() {
            CHANNEL_HTTP => Some(Self::Http),
            CHANNEL_WEBDAV => Some(Self::WebDav),
            CHANNEL_P2P => Some(Self::P2p),
            _ => None,
        }
    }

    /// 写回配置用的 id（与 [`Self::from_id`] 互逆）。
    pub fn id(&self) -> &'static str {
        match self {
            Self::Http => CHANNEL_HTTP,
            Self::WebDav => CHANNEL_WEBDAV,
            Self::P2p => CHANNEL_P2P,
        }
    }

    /// 界面展示名。
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Http => "HTTP 服务",
            Self::WebDav => "WebDAV",
            Self::P2p => "局域网直连",
        }
    }
}

/// 规范化通道列表：去空、去重、丢弃未知 id、保持顺序（顺序即优先级）。
///
/// 配置可能被手工编辑，重复 id 会让「首个优先」出现两个候选，切换时来回横跳；
/// 未知 id（旧版插件残留）留着只会让引擎选中一条不存在的通道。
pub fn normalize(channels: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(channels.len());
    for id in channels {
        let Some(channel) = Channel::from_id(id) else {
            tracing::warn!(channel = id, "未知同步通道，已忽略");
            continue;
        };
        if out.iter().any(|seen| seen == channel.id()) {
            continue;
        }
        out.push(channel.id().to_string());
    }
    out
}

/// 默认通道列表：沿用后台的 HTTP 本机 server，保证旧用户升级后同步不中断。
pub fn default_channels() -> Vec<String> {
    vec![CHANNEL_HTTP.to_string()]
}

/// 通道健康度。
///
/// 连接状态是秒级的瞬时值，直接拿它做切换会横跳：这里用两个时间窗——连上要稳定
/// [`P2P_STABLE`] 才算健康，掉线要超过 [`P2P_LOST_GRACE`] 才算不可用。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    connected: bool,
    up_since: Option<Instant>,
    down_since: Option<Instant>,
}

impl Health {
    /// 连接状态翻转。
    ///
    /// 同值重复上报直接忽略：调用方每个轮询周期都会拿到 `Event::Peer`，若每次都
    /// 刷新时间窗，「连上 5 秒算稳定」永远等不到。
    pub fn note(&mut self, connected: bool, now: Instant) {
        if connected == self.connected {
            return;
        }
        self.connected = connected;
        if connected {
            self.up_since = Some(now);
            self.down_since = None;
        } else {
            self.up_since = None;
            self.down_since = Some(now);
        }
    }

    /// 通道刚启用：从「未连接」开始计宽限期，否则永远等不到降级。
    pub fn started(&mut self, now: Instant) {
        self.connected = false;
        self.up_since = None;
        self.down_since = Some(now);
    }

    /// 已连上且稳定超过 [`P2P_STABLE`]。
    pub fn stable(&self, now: Instant) -> bool {
        self.connected
            && self
                .up_since
                .is_some_and(|t| now.duration_since(t) >= P2P_STABLE)
    }

    /// 未连上且超过 [`P2P_LOST_GRACE`]（含「启用后一直没连上」）。
    pub fn stale(&self, now: Instant) -> bool {
        !self.connected
            && self
                .down_since
                .is_some_and(|t| now.duration_since(t) >= P2P_LOST_GRACE)
    }
}

/// 决定这一轮该跑哪条通道；`None` 表示没有可用通道（同步关闭）。
///
/// 规则（与 XimeYi / Android 一致）：
/// - 没有当前通道 → 取列表首个；
/// - 当前就是首选 → 首选是 P2P 且已不可用 → 退到第二条（没有备用就保持）；
/// - 当前是备用 → 首选是 P2P、已恢复稳定、且过了驻留期 → 切回。
///
/// 只对 P2P 做健康判定，理由见模块文档。
pub fn decide(
    channels: &[String],
    current: Option<&str>,
    health: &Health,
    dwell_ok: bool,
    now: Instant,
) -> Option<String> {
    let preferred = channels.first()?;
    let Some(current) = current else {
        return Some(preferred.clone());
    };
    if current == preferred {
        if preferred == CHANNEL_P2P && health.stale(now) {
            return Some(
                channels
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| preferred.clone()),
            );
        }
        return Some(preferred.clone());
    }
    // 正在备用通道：首选恢复且稳定，且过了驻留期才切回。
    if preferred == CHANNEL_P2P && health.stable(now) && dwell_ok {
        return Some(preferred.clone());
    }
    Some(current.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn h() -> Health {
        Health::default()
    }

    #[test]
    fn 内置通道_id_可互逆解析() {
        for id in [CHANNEL_HTTP, CHANNEL_WEBDAV, CHANNEL_P2P] {
            let channel = Channel::from_id(id).expect("内置通道应可识别");
            assert_eq!(channel.id(), id);
            assert!(!channel.display_name().is_empty());
        }
        // 未知 id（旧版插件残留）必须被识别为「不存在」，不能当成可用通道
        assert!(Channel::from_id("com.example.sync").is_none());
        assert!(Channel::from_id("").is_none());
    }

    #[test]
    fn 规范化去空去重并保序() {
        assert_eq!(
            normalize(&ids(&["p2p", "", " webdav ", "p2p", "http"])),
            ids(&["p2p", "webdav", "http"])
        );
        assert!(normalize(&[]).is_empty());
    }

    #[test]
    fn 规范化丢弃未知通道() {
        assert_eq!(
            normalize(&ids(&["com.example.sync", "webdav"])),
            ids(&["webdav"])
        );
        assert!(normalize(&ids(&["com.example.sync"])).is_empty());
    }

    #[test]
    fn 通道列表为空时不选通道() {
        assert!(decide(&[], None, &h(), true, Instant::now()).is_none());
    }

    #[test]
    fn 首次直接用首选通道() {
        let now = Instant::now();
        assert_eq!(
            decide(&ids(&["p2p", "webdav"]), None, &h(), true, now),
            Some("p2p".to_string())
        );
        assert_eq!(
            decide(&ids(&["webdav", "p2p"]), None, &h(), true, now),
            Some("webdav".to_string())
        );
    }

    #[test]
    fn 首选掉线超过宽限才退到备用() {
        let now = Instant::now();
        let mut health = Health::default();
        health.started(now);
        let channels = ids(&["p2p", "webdav"]);
        // 宽限期内保持
        assert_eq!(
            decide(
                &channels,
                Some("p2p"),
                &health,
                true,
                now + P2P_LOST_GRACE / 2
            ),
            Some("p2p".to_string())
        );
        // 超过宽限 → 备用
        assert_eq!(
            decide(&channels, Some("p2p"), &health, true, now + P2P_LOST_GRACE),
            Some("webdav".to_string())
        );
        // 没有备用就保持（节点还在，可能只是对端离线）
        assert_eq!(
            decide(
                &ids(&["p2p"]),
                Some("p2p"),
                &health,
                true,
                now + P2P_LOST_GRACE
            ),
            Some("p2p".to_string())
        );
    }

    #[test]
    fn 备用通道上等首选稳定且过驻留才切回() {
        let now = Instant::now();
        let mut health = Health::default();
        health.started(now);
        health.note(true, now);
        let channels = ids(&["p2p", "webdav"]);
        // 刚连上不稳定 → 留在备用
        assert_eq!(
            decide(&channels, Some("webdav"), &health, true, now),
            Some("webdav".to_string())
        );
        // 稳定但驻留未满 → 留在备用
        assert_eq!(
            decide(&channels, Some("webdav"), &health, false, now + P2P_STABLE),
            Some("webdav".to_string())
        );
        // 稳定 + 驻留满足 → 切回
        assert_eq!(
            decide(&channels, Some("webdav"), &health, true, now + P2P_STABLE),
            Some("p2p".to_string())
        );
    }

    #[test]
    fn 首选不是_p2p_时不因健康度切换() {
        let now = Instant::now();
        let mut health = Health::default();
        health.started(now);
        health.note(true, now);
        assert_eq!(
            decide(
                &ids(&["webdav", "p2p"]),
                Some("webdav"),
                &health,
                true,
                now + P2P_STABLE * 10
            ),
            Some("webdav".to_string())
        );
    }

    #[test]
    fn 健康度要用时间窗判定() {
        let now = Instant::now();
        let mut health = Health::default();
        health.started(now);
        assert!(!health.stable(now));
        assert!(!health.stale(now));
        assert!(health.stale(now + P2P_LOST_GRACE));

        // 同值重复上报不得刷新时间窗，否则「稳定 5 秒」永远等不到
        let up = now + P2P_LOST_GRACE;
        health.note(true, up);
        health.note(true, up + P2P_STABLE / 2);
        assert!(!health.stable(up + P2P_STABLE / 2));
        assert!(health.stable(up + P2P_STABLE));
        assert!(!health.stale(up + P2P_STABLE));
    }
}
