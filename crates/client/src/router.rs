//! 通道路由：把「通道列表 + 降级规则」包装成一个 [`Transport`]。
//!
//! 引擎只认一个 `Arc<dyn Transport>`，通道切换因此必须对引擎透明：本地轮询、去重、
//! 回声抑制、push 退避都在引擎里，改动通道不该让它们重置。路由层只做两件事：
//!
//! 1. **选通道**：每轮调用前按 [`channel::decide`] 复核当前通道是否还该用；
//! 2. **转发**：`push` / `pull` / `next_event` 全部交给当前通道。
//!
//! 切换判据只来自 P2P 的连接态（由设备层的 `Event::Peer` 维护）。HTTP / WebDAV
//! 一次失败可能只是网络抖动，不是通道不可用，理由见 [`crate::channel`] 模块文档。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use xime_sync_domain::profile::Profile;

use crate::channel::{self, Health, MIN_DWELL};
use crate::transport::{Transport, TransportError};

/// 单个通道条目。
struct Entry {
    id: String,
    transport: Arc<dyn Transport>,
}

/// 路由状态（当前通道 + 健康窗口 + 驻留起点）。
struct State {
    current: usize,
    switched_at: Instant,
    health: Health,
}

/// 连接态探针：P2P 通道靠它判断「有没有在线设备」。
pub type Probe = Box<dyn Fn() -> bool + Send + Sync>;

/// 多通道路由。
pub struct Router {
    entries: Vec<Entry>,
    state: Mutex<State>,
    probe: Probe,
    /// 当前通道索引（无锁读，供状态上报用）。
    current: AtomicUsize,
}

impl Router {
    /// 用编译期可用的连接态探针构建（`p2p` feature 关闭时恒为 `false`）。
    pub fn new(entries: Vec<(String, Arc<dyn Transport>)>) -> Self {
        Self::with_probe(entries, default_probe())
    }

    /// 注入探针（测试用）。
    pub fn with_probe(entries: Vec<(String, Arc<dyn Transport>)>, probe: Probe) -> Self {
        let entries: Vec<Entry> = entries
            .into_iter()
            .map(|(id, transport)| Entry { id, transport })
            .collect();
        Self {
            entries,
            state: Mutex::new(State {
                current: 0,
                switched_at: Instant::now(),
                health: Health::default(),
            }),
            probe,
            current: AtomicUsize::new(0),
        }
    }

    /// 通道为空（没有任何可用通道）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 当前通道 id（UI 显示用）。
    pub fn current_id(&self) -> Option<String> {
        self.entries
            .get(self.current.load(Ordering::Relaxed))
            .map(|e| e.id.clone())
    }

    /// 复核并可能切换当前通道，返回本轮要用的 transport。
    ///
    /// 首次调用时 `Health` 需要从「未连接」开始计宽限期，否则 P2P 一直没连上时
    /// 永远等不到降级（`Health::default()` 的两个时间窗都是 `None`）。
    fn select(&self) -> Option<Arc<dyn Transport>> {
        let ids: Vec<String> = self.entries.iter().map(|e| e.id.clone()).collect();
        if ids.is_empty() {
            return None;
        }
        let now = Instant::now();
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.health == Health::default() {
            state.health.started(now);
        }
        state.health.note((self.probe)(), now);
        let dwell_ok = now.duration_since(state.switched_at) >= MIN_DWELL;
        let current_id = ids.get(state.current).cloned();
        let decided = channel::decide(&ids, current_id.as_deref(), &state.health, dwell_ok, now);
        // 只在「决策结果与当前不同」时更新状态：同值时刷新 switched_at 会让驻留期
        // 永远满足不了，备用通道刚切过去就可能又切回来。
        if let Some(next) = decided
            && current_id.as_deref() != Some(next.as_str())
            && let Some(idx) = ids.iter().position(|id| *id == next)
        {
            tracing::info!(
                from = current_id.as_deref().unwrap_or("-"),
                to = %next,
                "同步通道已切换"
            );
            state.current = idx;
            state.switched_at = now;
            self.current.store(idx, Ordering::Relaxed);
        }
        self.entries
            .get(state.current)
            .map(|e| Arc::clone(&e.transport))
    }
}

/// 无 `p2p` feature 时没有连接态来源，恒为未连接（此时 P2P 通道本身也不会被装配）。
fn default_probe() -> Probe {
    #[cfg(feature = "p2p")]
    {
        Box::new(crate::p2p::is_connected)
    }
    #[cfg(not(feature = "p2p"))]
    {
        Box::new(|| false)
    }
}

#[async_trait]
impl Transport for Router {
    async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
        let Some(transport) = self.select() else {
            return Err(TransportError::Connect("没有可用的同步通道".to_string()));
        };
        transport.push(profile).await
    }

    async fn pull(&self) -> Result<Option<Profile>, TransportError> {
        let Some(transport) = self.select() else {
            return Err(TransportError::Connect("没有可用的同步通道".to_string()));
        };
        transport.pull().await
    }

    async fn next_event(&self, timeout: Duration) -> Result<Option<Profile>, TransportError> {
        let Some(transport) = self.select() else {
            return Err(TransportError::Connect("没有可用的同步通道".to_string()));
        };
        transport.next_event(timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 记录调用的假通道。
    struct Fake {
        id: &'static str,
        calls: Arc<Mutex<Vec<&'static str>>>,
        pushes: Arc<AtomicUsize>,
        fail: bool,
    }

    #[async_trait]
    impl Transport for Fake {
        async fn push(&self, _profile: &Profile) -> Result<(), TransportError> {
            self.calls.lock().unwrap().push(self.id);
            self.pushes.fetch_add(1, Ordering::Relaxed);
            if self.fail {
                return Err(TransportError::Send("boom".to_string()));
            }
            Ok(())
        }

        async fn pull(&self) -> Result<Option<Profile>, TransportError> {
            Ok(None)
        }

        async fn next_event(&self, _timeout: Duration) -> Result<Option<Profile>, TransportError> {
            Ok(None)
        }
    }

    fn fake(
        id: &'static str,
        calls: &Arc<Mutex<Vec<&'static str>>>,
    ) -> (String, Arc<dyn Transport>) {
        (
            id.to_string(),
            Arc::new(Fake {
                id,
                calls: Arc::clone(calls),
                pushes: Arc::new(AtomicUsize::new(0)),
                fail: false,
            }),
        )
    }

    #[tokio::test]
    async fn 首个通道优先() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let router = Router::with_probe(
            vec![fake("webdav", &calls), fake("http", &calls)],
            Box::new(|| false),
        );
        router.push(&Profile::from_text("hi", None)).await.unwrap();
        assert_eq!(router.current_id().as_deref(), Some("webdav"));
        assert_eq!(*calls.lock().unwrap(), vec!["webdav"]);
    }

    #[tokio::test]
    async fn 无通道时_push_报错而不是静默成功() {
        let router = Router::with_probe(Vec::new(), Box::new(|| false));
        assert!(router.is_empty());
        assert!(router.push(&Profile::from_text("hi", None)).await.is_err());
        assert!(matches!(
            router.pull().await,
            Err(TransportError::Connect(_))
        ));
    }

    /// P2P 排首位但一直没连上：过宽限期后自动退到第二条，且之后复用备用通道。
    #[tokio::test]
    async fn p2p_未连接时退到备用通道() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let router = Router::with_probe(
            vec![fake("p2p", &calls), fake("webdav", &calls)],
            Box::new(|| false),
        );
        // 首轮：宽限期内仍在 p2p（选择发生但先按首选跑）
        router.push(&Profile::from_text("a", None)).await.unwrap();
        assert_eq!(router.current_id().as_deref(), Some("p2p"));

        // 时间窗不能靠真实睡眠：直接把状态里的 down_since 往前拨
        {
            let mut state = router.state.lock().unwrap();
            state
                .health
                .started(Instant::now() - channel::P2P_LOST_GRACE);
        }
        router.push(&Profile::from_text("b", None)).await.unwrap();
        assert_eq!(router.current_id().as_deref(), Some("webdav"));
        assert_eq!(*calls.lock().unwrap(), vec!["p2p", "webdav"]);
    }
}
