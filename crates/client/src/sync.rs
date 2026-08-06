//! 同步引擎：本地剪贴板 ↔ 服务器 双向同步。
//!
//! 规则（三通道统一去重语义，见设计文档 3.5）：
//! - 本地轮询发现变化 → 与上次推送的 hash 不同 → 推送到服务器（clipboard.set / PUT）
//! - 收到服务器变更 → 与本地 hash 及"自写 hash"比对 → 不同才写回本地剪贴板
//! - 自写抑制：引擎写入本地后记录 hash，轮询到同一 hash 判定为回声，跳过推送
//!
//! 引擎是纯逻辑层，不依赖具体剪贴板/传输实现，便于单元测试。

use std::sync::Arc;
use std::time::Duration;

use domain::hash::text_hash;
use domain::profile::{Profile, ProfileType};

use crate::clipboard::ClipboardAdapter;
use crate::config::ClientConfig;
use crate::transport::{HttpTransport, Transport, WsTransport};

/// 推送失败后的重试退避间隔：避免 server 未启动时每轮轮询都重试刷日志。
const PUSH_RETRY_BACKOFF: Duration = Duration::from_secs(5);

/// client 与 server 的连接状态（引擎上报，供桌面 UI 提示用户）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// 已连接：push 成功或启动拉取成功。
    Online,
    /// 连接失败：本地 server 未启动或远程不可达。
    Offline,
}

/// 一次同步决策的结果（供测试断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// 无动作。
    None,
    /// 推送本地 profile 到服务器。
    Push(Profile),
    /// 从服务器写入本地剪贴板。
    Write(String),
}

/// 同步引擎。
pub struct SyncEngine {
    cfg: ClientConfig,
    /// 保留：WS 断线重连时重建传输通道需要密码。
    #[allow(dead_code)]
    password: String,
    clipboard: Arc<dyn ClipboardAdapter>,
    transport: Arc<dyn Transport>,
    /// 上次推送/写入的 hash（去重）。
    last_hash: Option<String>,
    /// 自写 hash：引擎写入本地剪贴板的内容 hash，用于回声抑制。
    self_written: Option<String>,
    /// 推送失败后的退避截止时间：窗口内不重复尝试同一内容。
    retry_until: Option<std::time::Instant>,
    /// 连接状态回调（桌面形态注入：本地模式提示开启 server，远程模式提示不可用）。
    status_handler: Option<Arc<dyn Fn(SyncStatus) + Send + Sync>>,
}

impl SyncEngine {
    /// 构造引擎。
    pub fn new(cfg: ClientConfig, password: String, clipboard: Arc<dyn ClipboardAdapter>) -> Self {
        Self {
            transport: make_transport(&cfg, &password),
            cfg,
            password,
            clipboard,
            last_hash: None,
            self_written: None,
            retry_until: None,
            status_handler: None,
        }
    }

    /// 注入连接状态回调（桌面 UI 提示用，引擎内不感知 UI）。
    pub fn set_status_handler(&mut self, handler: Arc<dyn Fn(SyncStatus) + Send + Sync>) {
        self.status_handler = Some(handler);
    }

    fn report_status(&self, status: SyncStatus) {
        if let Some(h) = &self.status_handler {
            h(status);
        }
    }

    /// 主循环：轮询本地 + 接收远端（WS 实时或 HTTP 轮询）。
    pub async fn run(&mut self) {
        // 启动即拉取一次服务器快照（若本地为空则采纳服务器内容），并上报连接状态
        match self.transport.pull().await {
            Ok(Some(remote)) => {
                self.report_status(SyncStatus::Online);
                if remote.r#type == ProfileType::Text {
                    let local = self.clipboard.get();
                    let should_adopt = match local {
                        None => true,
                        Some(l) => text_hash(&l) != remote.hash,
                    };
                    if should_adopt {
                        let action = self.apply_remote(&remote);
                        if matches!(action, SyncAction::Write(_)) {
                            tracing::debug!("adopted server clipboard on start");
                        }
                    }
                }
            }
            Ok(None) => self.report_status(SyncStatus::Online),
            Err(_) => self.report_status(SyncStatus::Offline),
        }

        let poll = Duration::from_millis(self.cfg.poll_ms);
        let recv_timeout = poll.max(Duration::from_millis(100));
        loop {
            // 分支 1：本地剪贴板轮询
            let action = self.on_local_poll(self.clipboard.get());
            self.execute(action).await;
            // 分支 2：远端事件（WS 阻塞接收 / HTTP 睡眠后拉取）
            match self.transport.next_event(recv_timeout).await {
                Ok(Some(remote)) => {
                    let action = self.apply_remote(&remote);
                    self.execute(action).await;
                }
                Ok(None) => {}
                Err(_) => {
                    // 连接问题：HTTP 通道 next_event 只在 push/pull 失败时返回 Err，
                    // 这里静默等下一个轮询周期；WS 的重连由上层（app）处理。
                    tokio::time::sleep(poll).await;
                }
            }
        }
    }

    /// 本地轮询决策（纯逻辑，可单测）：剪贴板变化且非自写回声 → Push。
    pub fn on_local_poll(&mut self, local: Option<String>) -> SyncAction {
        // 推送失败退避窗口内不重复尝试，避免 server 未启动时每轮轮询都重试刷日志
        if let Some(deadline) = self.retry_until {
            if std::time::Instant::now() < deadline {
                return SyncAction::None;
            }
            // 退避结束：撤销 last_hash，允许重试上次失败的内容
            self.retry_until = None;
            self.last_hash = None;
        }
        let Some(text) = local else {
            return SyncAction::None;
        };
        if text.is_empty() {
            return SyncAction::None;
        }
        let hash = text_hash(&text);
        // 自写回声：内容 hash 与自写一致 → 跳过
        if self.self_written.as_deref() == Some(&hash) {
            return SyncAction::None;
        }
        if self.last_hash.as_deref() == Some(&hash) {
            return SyncAction::None;
        }
        self.last_hash = Some(hash);
        SyncAction::Push(Profile::from_text(text, Some(self.cfg.source.clone())))
    }

    /// 远端变更决策（纯逻辑，可单测）：hash 与本地/自写都不同 → Write。
    pub fn apply_remote(&mut self, remote: &Profile) -> SyncAction {
        if remote.r#type != ProfileType::Text || remote.text.is_empty() {
            return SyncAction::None;
        }
        let local = self.clipboard.get();
        let local_hash = local.as_deref().map(text_hash);
        if local_hash.as_deref() == Some(&remote.hash) {
            // 本地已是该内容，仅记录 last_hash 避免重复推送
            self.last_hash = Some(remote.hash.clone());
            return SyncAction::None;
        }
        if self.self_written.as_deref() == Some(&remote.hash) {
            // 自写内容被服务器广播回来 → 忽略
            self.last_hash = Some(remote.hash.clone());
            return SyncAction::None;
        }
        self.self_written = Some(remote.hash.clone());
        self.last_hash = Some(remote.hash.clone());
        SyncAction::Write(remote.text.clone())
    }

    /// 执行同步动作（写剪贴板为同步操作；push 异步）。
    pub async fn execute(&mut self, action: SyncAction) {
        match action {
            SyncAction::None => {}
            SyncAction::Push(profile) => {
                match self.transport.push(&profile).await {
                    Ok(_) => {
                        self.report_status(SyncStatus::Online);
                    }
                    Err(e) => {
                        // server 未启动/不可达属预期场景，不在 warn 刷屏；
                        // 状态经回调上报 UI，由 UI 按 SyncMode 提示用户。
                        tracing::debug!("push failed: {e}");
                        // 退避重试：间隔内不再尝试，server 起来后自动补推
                        self.retry_until = Some(std::time::Instant::now() + PUSH_RETRY_BACKOFF);
                        self.report_status(SyncStatus::Offline);
                    }
                }
            }
            SyncAction::Write(text) => {
                self.clipboard.set(&text);
            }
        }
    }
}

/// 按配置构建传输通道（WS 实时，默认开启；关闭则用 HTTP 轮询）。
fn make_transport(cfg: &ClientConfig, password: &str) -> Arc<dyn Transport> {
    if cfg.ws_enabled {
        Arc::new(WsTransport::new(cfg, password))
    } else {
        Arc::new(HttpTransport::new(cfg, password))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::MockClipboard;
    use crate::transport::TransportError;
    use std::sync::Mutex;

    use async_trait::async_trait;

    /// 测试用传输：记录推送、提供可编程的远端 profile。
    struct MockTransport {
        pushed: Mutex<Vec<Profile>>,
        remote: Mutex<Option<Profile>>,
        fail_pull: Mutex<bool>,
        fail_push: Mutex<bool>,
    }

    impl MockTransport {
        fn new(remote: Option<Profile>) -> Self {
            Self {
                pushed: Mutex::new(vec![]),
                remote: Mutex::new(remote),
                fail_pull: Mutex::new(false),
                fail_push: Mutex::new(false),
            }
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn push(&self, profile: &Profile) -> Result<(), TransportError> {
            if *self.fail_push.lock().unwrap() {
                return Err(TransportError::Send("offline".into()));
            }
            self.pushed.lock().unwrap().push(profile.clone());
            // 模拟服务器广播回写：设置远端为该内容
            *self.remote.lock().unwrap() = Some(profile.clone());
            Ok(())
        }

        async fn pull(&self) -> Result<Option<Profile>, TransportError> {
            if *self.fail_pull.lock().unwrap() {
                return Err(TransportError::Recv("offline".into()));
            }
            Ok(self.remote.lock().unwrap().clone())
        }

        async fn next_event(&self, _timeout: Duration) -> Result<Option<Profile>, TransportError> {
            Ok(self.remote.lock().unwrap().clone())
        }
    }

    fn cfg(source: &str) -> ClientConfig {
        ClientConfig {
            server_url: "http://127.0.0.1:8443".to_string(),
            username: "alice".to_string(),
            password: None,
            password_env: "XIMED_CLIENT_PASSWORD".to_string(),
            source: source.to_string(),
            poll_ms: 10,
            http_fallback_ms: 10,
            heartbeat_secs: 30,
            ws_enabled: true,
        }
    }

    /// 构造引擎并注入 MockTransport。
    fn engine_with_transport(
        source: &str,
        transport: Arc<dyn Transport>,
    ) -> (SyncEngine, Arc<MockClipboard>) {
        let cb = Arc::new(MockClipboard::new());
        let mut e = SyncEngine::new(cfg(source), "pw".to_string(), cb.clone());
        e.transport = transport;
        (e, cb)
    }

    #[test]
    fn local_change_triggers_push_and_echo_is_suppressed() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        // 启动 pull 服务器为空，无动作
        cb.simulate_external_copy("hello");
        let a = e.on_local_poll(cb.get());
        assert!(matches!(a, SyncAction::Push(p) if p.hash == text_hash("hello")));
        assert_eq!(e.last_hash.as_deref(), Some(text_hash("hello").as_str()));
        // 回声：服务器把内容广播回来（模拟 apply_remote，远端内容即刚推送的 profile）
        let pushed = Profile::from_text("hello", Some("dev-a".to_string()));
        let a2 = e.apply_remote(&pushed);
        assert_eq!(a2, SyncAction::None, "self-written echo must be ignored");
    }

    #[test]
    fn remote_change_writes_local_and_records_self_written() {
        let remote = Profile::from_text("from-server", Some("dev-b".to_string()));
        let t = Arc::new(MockTransport::new(Some(remote)));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        let a = e.apply_remote(&t.remote.lock().unwrap().clone().unwrap());
        match &a {
            SyncAction::Write(txt) => assert_eq!(txt, "from-server"),
            other => panic!("expected write, got {other:?}"),
        }
        // 决策确认后执行写本地（Write 分支为同步操作）
        if let SyncAction::Write(text) = a {
            e.clipboard.set(&text);
        }
        assert_eq!(cb.get().as_deref(), Some("from-server"));
        // 自写抑制：本地轮询到同一 hash 不应再推送
        let a2 = e.on_local_poll(cb.get());
        assert_eq!(a2, SyncAction::None);
    }

    #[test]
    fn unchanged_local_does_not_push_again() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        cb.simulate_external_copy("hello");
        let a = e.on_local_poll(cb.get());
        assert!(matches!(a, SyncAction::Push(_)));
        // 本地未变：不重复推送
        let a2 = e.on_local_poll(cb.get());
        assert_eq!(a2, SyncAction::None);
    }

    #[test]
    fn empty_local_is_ignored() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        let a = e.on_local_poll(cb.get());
        assert_eq!(a, SyncAction::None);
    }

    #[tokio::test]
    async fn failed_push_is_retried_after_backoff() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        cb.simulate_external_copy("hello");

        // 推送失败：进入退避窗口，且不立即重试
        *t.fail_push.lock().unwrap() = true;
        let a = e.on_local_poll(cb.get());
        assert!(matches!(a, SyncAction::Push(_)));
        e.execute(a).await;
        assert!(e.retry_until.is_some(), "failure must arm retry backoff");

        // 退避窗口内：不再尝试
        let a_skip = e.on_local_poll(cb.get());
        assert_eq!(a_skip, SyncAction::None);

        // 退避结束后：重试成功，且不重复推送
        e.retry_until = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        *t.fail_push.lock().unwrap() = false;
        let a2 = e.on_local_poll(cb.get());
        assert!(matches!(a2, SyncAction::Push(_)));
        e.execute(a2).await;
        assert_eq!(t.pushed.lock().unwrap().len(), 1);
        assert_eq!(e.last_hash.as_deref(), Some(text_hash("hello").as_str()));

        let a3 = e.on_local_poll(cb.get());
        assert_eq!(a3, SyncAction::None);
    }

    #[tokio::test]
    async fn status_handler_reports_offline_and_online() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        cb.simulate_external_copy("hello");

        // 注入状态回调（UI 提示用）
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        e.set_status_handler(Arc::new(move |s: SyncStatus| {
            seen_cb.lock().unwrap().push(s);
        }));

        // 推送失败 → Offline
        *t.fail_push.lock().unwrap() = true;
        let a = e.on_local_poll(cb.get());
        e.execute(a).await;
        assert_eq!(seen.lock().unwrap().as_slice(), &[SyncStatus::Offline]);

        // 退避结束后重试成功 → Online
        e.retry_until = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        *t.fail_push.lock().unwrap() = false;
        let a2 = e.on_local_poll(cb.get());
        e.execute(a2).await;
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[SyncStatus::Offline, SyncStatus::Online]
        );
    }
}
