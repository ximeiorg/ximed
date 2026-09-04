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

use xime_sync_domain::hash::{sha256_hex, text_hash};
use xime_sync_domain::profile::{Profile, ProfileType};

use crate::clipboard::{ClipboardAdapter, ImageData};
use crate::config::ClientConfig;
use crate::settings::SyncSettings;
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
    /// 从服务器写入本地剪贴板文本（携带来源设备，供通知）。
    Write {
        text: String,
        source: Option<String>,
    },
    /// 从服务器写入本地剪贴板图片（RGBA8 像素，携带来源设备，供通知）。
    WriteImage {
        width: u32,
        height: u32,
        data: Vec<u8>,
        source: Option<String>,
    },
}

/// 同步引擎。
#[allow(clippy::type_complexity)]
pub struct SyncEngine {
    cfg: ClientConfig,
    /// 同步开关与上传白名单设置。
    sync: SyncSettings,
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
    /// 下载通知回调（收到远端内容且通知开关开启时调用，携带来源设备）。
    download_handler: Option<Arc<dyn Fn(Option<String>) + Send + Sync>>,
    /// 历史记录回调（push 成功 / 写入本地剪贴板时调用，携带 profile）。
    /// 桌面形态注入：server 模式记录到 server（引擎不感知），直连模式记录到本地库。
    history_handler: Option<Arc<dyn Fn(&Profile) + Send + Sync>>,
}

impl SyncEngine {
    /// 构造引擎。
    pub fn new(
        cfg: ClientConfig,
        sync: SyncSettings,
        password: String,
        clipboard: Arc<dyn ClipboardAdapter>,
    ) -> Self {
        Self {
            transport: make_transport(&cfg, &password),
            cfg,
            sync,
            password,
            clipboard,
            last_hash: None,
            self_written: None,
            retry_until: None,
            status_handler: None,
            download_handler: None,
            history_handler: None,
        }
    }

    /// 注入连接状态回调（桌面 UI 提示用，引擎内不感知 UI）。
    pub fn set_status_handler(&mut self, handler: Arc<dyn Fn(SyncStatus) + Send + Sync>) {
        self.status_handler = Some(handler);
    }

    /// 注入下载通知回调（收到远端内容时调用，携带来源设备）。
    pub fn set_download_handler(&mut self, handler: Arc<dyn Fn(Option<String>) + Send + Sync>) {
        self.download_handler = Some(handler);
    }

    /// 注入历史记录回调（push 成功 / 写入本地时调用）。
    pub fn set_history_handler(&mut self, handler: Arc<dyn Fn(&Profile) + Send + Sync>) {
        self.history_handler = Some(handler);
    }

    fn report_status(&self, status: SyncStatus) {
        if let Some(h) = &self.status_handler {
            h(status);
        }
    }

    /// 主循环：轮询本地 + 接收远端（WS 实时或 HTTP 轮询）。
    pub async fn run(&mut self) {
        // 启动即拉取一次服务器快照（若本地为空则采纳服务器内容），并上报连接状态
        if self.sync.switch_on && self.sync.pull_switch_on {
            self.run_startup_pull().await;
        }

        let poll = Duration::from_millis(self.cfg.poll_ms);
        let recv_timeout = poll.max(Duration::from_millis(100));
        loop {
            // 分支 1：本地剪贴板轮询（图片优先；macOS 复制图片常同时带文本，同步图片更符合意图）
            let image = self.clipboard.get_image();
            let action = match image {
                Some(img) => self.on_image_poll(Some(img)),
                None => self.on_local_poll(self.clipboard.get()),
            };
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

    /// 启动拉取：主开关与拉取开关开启时尝试采纳服务器快照，并上报连接状态。
    async fn run_startup_pull(&mut self) {
        match self.transport.pull().await {
            Ok(Some(remote)) => {
                self.report_status(SyncStatus::Online);
                match remote.r#type {
                    ProfileType::Text => {
                        let local = self.clipboard.get();
                        let should_adopt = match local {
                            None => true,
                            Some(l) => text_hash(&l) != remote.hash,
                        };
                        if should_adopt {
                            let action = self.apply_remote(&remote);
                            if matches!(action, SyncAction::Write { .. }) {
                                tracing::debug!("adopted server clipboard on start");
                            }
                            self.execute(action).await;
                        }
                    }
                    ProfileType::Image => {
                        let local_hash = self.clipboard.get_image().map(|i| sha256_hex(&i.bytes));
                        let should_adopt = local_hash.as_deref() != Some(remote.hash.as_str());
                        if should_adopt {
                            let action = self.apply_remote(&remote);
                            if matches!(action, SyncAction::WriteImage { .. }) {
                                tracing::debug!("adopted server image on start");
                            }
                            self.execute(action).await;
                        }
                    }
                    _ => {}
                }
            }
            Ok(None) => self.report_status(SyncStatus::Online),
            Err(_) => self.report_status(SyncStatus::Offline),
        }
    }

    /// 推送退避检查：退避窗口内返回 true（不推送）；窗口结束后撤销 last_hash 允许重试。
    fn push_backoff_active(&mut self) -> bool {
        if let Some(deadline) = self.retry_until {
            if std::time::Instant::now() < deadline {
                return true;
            }
            // 退避结束：撤销 last_hash，允许重试上次失败的内容
            self.retry_until = None;
            self.last_hash = None;
        }
        false
    }

    /// 本地文本轮询决策（纯逻辑，可单测）：剪贴板变化且非自写回声 → Push。
    pub fn on_local_poll(&mut self, local: Option<String>) -> SyncAction {
        // 同步主开关或推送开关关闭：不推送
        if !self.sync.switch_on || !self.sync.push_switch_on {
            return SyncAction::None;
        }
        // 推送失败退避窗口内不重复尝试，避免 server 未启动时每轮轮询都重试刷日志
        if self.push_backoff_active() {
            return SyncAction::None;
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
        let profile = Profile::from_text(text, Some(self.cfg.source.clone()));
        if !self.sync.type_allowed(profile.r#type) {
            return SyncAction::None;
        }
        if profile.size as u64 > self.sync.max_file_byte {
            return SyncAction::None;
        }
        self.last_hash = Some(hash);
        SyncAction::Push(profile)
    }

    /// 本地图片轮询决策（纯逻辑，可单测）：剪贴板图片变化且非自写回声 → Push。
    pub fn on_image_poll(&mut self, image: Option<ImageData>) -> SyncAction {
        // 同步主开关或推送开关关闭：不推送
        if !self.sync.switch_on || !self.sync.push_switch_on {
            return SyncAction::None;
        }
        // 推送失败退避窗口内不重复尝试
        if self.push_backoff_active() {
            return SyncAction::None;
        }
        let Some(image) = image else {
            return SyncAction::None;
        };
        if image.bytes.is_empty() {
            return SyncAction::None;
        }
        let hash = sha256_hex(&image.bytes);
        // 自写回声：内容 hash 与自写一致 → 跳过
        if self.self_written.as_deref() == Some(&hash) {
            return SyncAction::None;
        }
        if self.last_hash.as_deref() == Some(&hash) {
            return SyncAction::None;
        }
        let profile = Profile::from_image(
            image.bytes.clone(),
            image.width,
            image.height,
            Some(self.cfg.source.clone()),
        );
        if !self.sync.type_allowed(profile.r#type) {
            return SyncAction::None;
        }
        if profile.size as u64 > self.sync.max_file_byte {
            return SyncAction::None;
        }
        self.last_hash = Some(hash);
        SyncAction::Push(profile)
    }

    /// 远端变更决策（纯逻辑，可单测）：hash 与本地/自写都不同 → Write。
    pub fn apply_remote(&mut self, remote: &Profile) -> SyncAction {
        // 同步主开关或拉取开关关闭：不写入本地
        if !self.sync.switch_on || !self.sync.pull_switch_on {
            return SyncAction::None;
        }
        match remote.r#type {
            ProfileType::Text => {
                if remote.text.is_empty() {
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
                SyncAction::Write {
                    text: remote.text.clone(),
                    source: remote.source.clone(),
                }
            }
            ProfileType::Image => {
                let Some(data) = remote.data.as_ref() else {
                    return SyncAction::None;
                };
                if data.is_empty() {
                    return SyncAction::None;
                }
                let local_hash = self.clipboard.get_image().map(|i| sha256_hex(&i.bytes));
                if local_hash.as_deref() == Some(&remote.hash) {
                    self.last_hash = Some(remote.hash.clone());
                    return SyncAction::None;
                }
                if self.self_written.as_deref() == Some(&remote.hash) {
                    self.last_hash = Some(remote.hash.clone());
                    return SyncAction::None;
                }
                self.self_written = Some(remote.hash.clone());
                self.last_hash = Some(remote.hash.clone());
                SyncAction::WriteImage {
                    width: remote.width.unwrap_or(0),
                    height: remote.height.unwrap_or(0),
                    data: data.clone(),
                    source: remote.source.clone(),
                }
            }
            ProfileType::File | ProfileType::Group | ProfileType::None => SyncAction::None,
        }
    }

    /// 执行同步动作（写剪贴板为同步操作；push 异步）。
    pub async fn execute(&mut self, action: SyncAction) {
        match action {
            SyncAction::None => {}
            SyncAction::Push(profile) => {
                match self.transport.push(&profile).await {
                    Ok(_) => {
                        self.report_status(SyncStatus::Online);
                        // 历史记录：推送成功即本机剪贴板内容生效
                        if let Some(h) = &self.history_handler {
                            h(&profile);
                        }
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
            SyncAction::Write { text, source } => {
                self.clipboard.set(&text);
                // 历史记录：远端内容写入本地剪贴板
                if let Some(h) = &self.history_handler {
                    h(&Profile::from_text(text.clone(), source.clone()));
                }
                // 通知开关：收到远端内容时告知 UI（来源设备）
                if self.sync.notify_on_downloaded
                    && let Some(h) = &self.download_handler
                {
                    h(source);
                }
            }
            SyncAction::WriteImage {
                width,
                height,
                data,
                source,
            } => {
                self.clipboard.set_image(&ImageData {
                    width,
                    height,
                    bytes: data.clone(),
                });
                // 历史记录：远端图片写入本地剪贴板
                if let Some(h) = &self.history_handler {
                    h(&Profile::from_image(
                        data.clone(),
                        width,
                        height,
                        source.clone(),
                    ));
                }
                // 通知开关：收到远端图片时告知 UI（来源设备）
                if self.sync.notify_on_downloaded
                    && let Some(h) = &self.download_handler
                {
                    h(source);
                }
            }
        }
    }
}

/// 按配置构建传输通道：
/// - server 适配器（默认）：WS 实时，关闭则 HTTP 轮询
/// - webdav / s3 适配器：直连远端存储（绕过 ximed server）
/// - plugin 适配器：Lua clipboard_sync 插件通道（需 `plugin` feature）
fn make_transport(cfg: &ClientConfig, password: &str) -> Arc<dyn Transport> {
    use crate::config::AdapterKind;
    match cfg.adapter {
        AdapterKind::WebDav | AdapterKind::S3 => {
            // 直连配置缺失时退化为空通道（引擎将按 Offline 处理）；由上层校验配置
            crate::direct::make_direct_transport(cfg)
                .unwrap_or_else(|_| Arc::new(crate::transport::HttpTransport::new(cfg, password)))
        }
        AdapterKind::Plugin => {
            #[cfg(feature = "plugin")]
            match crate::plugin::SyncPluginManager::new(&cfg.plugins_dir)
                .make_transport(&cfg.plugin_id)
            {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("plugin transport unavailable (id={}): {e}", cfg.plugin_id);
                    Arc::new(crate::transport::HttpTransport::new(cfg, password))
                }
            }
            #[cfg(not(feature = "plugin"))]
            {
                tracing::warn!("adapter=plugin requires the client 'plugin' feature");
                Arc::new(crate::transport::HttpTransport::new(cfg, password))
            }
        }
        AdapterKind::Server => {
            if cfg.ws_enabled {
                Arc::new(WsTransport::new(cfg, password))
            } else {
                Arc::new(HttpTransport::new(cfg, password))
            }
        }
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
            source: source.to_string(),
            poll_ms: 10,
            http_fallback_ms: 10,
            ..ClientConfig::default()
        }
    }

    /// 构造引擎并注入 MockTransport。
    fn engine_with_transport(
        source: &str,
        transport: Arc<dyn Transport>,
    ) -> (SyncEngine, Arc<MockClipboard>) {
        let cb = Arc::new(MockClipboard::new());
        // 测试默认开启同步主开关（产品默认关闭，由 UI/配置显式开启）
        let sync = SyncSettings {
            switch_on: true,
            ..SyncSettings::default()
        };
        let mut e = SyncEngine::new(cfg(source), sync, "pw".to_string(), cb.clone());
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
            SyncAction::Write { text, .. } => assert_eq!(text, "from-server"),
            other => panic!("expected write, got {other:?}"),
        }
        // 决策确认后执行写本地（Write 分支为同步操作）
        if let SyncAction::Write { text, .. } = a {
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

    #[test]
    fn master_switch_off_suppresses_push_and_pull() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = false;
        cb.simulate_external_copy("hello");
        // 主开关关：本地变化不推送
        assert_eq!(e.on_local_poll(cb.get()), SyncAction::None);
        // 主开关关：远端变化不写本地
        let remote = Profile::from_text("from-server", Some("dev-b".to_string()));
        assert_eq!(e.apply_remote(&remote), SyncAction::None);
    }

    #[test]
    fn push_switch_off_suppresses_upload_only() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.push_switch_on = false;
        cb.simulate_external_copy("hello");
        // 推送关：本地不推送
        assert_eq!(e.on_local_poll(cb.get()), SyncAction::None);
        // 拉取仍工作
        let remote = Profile::from_text("from-server", Some("dev-b".to_string()));
        assert!(matches!(
            e.apply_remote(&remote),
            SyncAction::Write { text, .. } if text == "from-server"
        ));
    }

    #[test]
    fn pull_switch_off_suppresses_download_only() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.pull_switch_on = false;
        cb.simulate_external_copy("hello");
        // 推送仍工作
        assert!(matches!(e.on_local_poll(cb.get()), SyncAction::Push(_)));
        // 拉取关：远端变化不写本地
        let remote = Profile::from_text("from-server", Some("dev-b".to_string()));
        assert_eq!(e.apply_remote(&remote), SyncAction::None);
    }

    #[test]
    fn upload_text_disabled_suppresses_push() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.upload_text = false;
        cb.simulate_external_copy("hello");
        assert_eq!(e.on_local_poll(cb.get()), SyncAction::None);
    }

    #[tokio::test]
    async fn download_notification_carries_source_when_enabled() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.notify_on_downloaded = true;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        e.set_download_handler(Arc::new(move |src: Option<String>| {
            seen_cb.lock().unwrap().push(src);
        }));
        let remote = Profile::from_text("from-server", Some("dev-b".to_string()));
        let a = e.apply_remote(&remote);
        e.execute(a).await;
        assert_eq!(cb.get().as_deref(), Some("from-server"));
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[Some("dev-b".to_string())],
            "download handler must receive source device"
        );
    }

    #[tokio::test]
    async fn download_notification_suppressed_when_disabled() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.notify_on_downloaded = false;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        e.set_download_handler(Arc::new(move |src: Option<String>| {
            seen_cb.lock().unwrap().push(src);
        }));
        let remote = Profile::from_text("from-server", Some("dev-b".to_string()));
        let a = e.apply_remote(&remote);
        e.execute(a).await;
        assert_eq!(cb.get().as_deref(), Some("from-server"));
        assert!(seen.lock().unwrap().is_empty());
    }

    // ---------- 图片同步 ----------

    #[test]
    fn local_image_triggers_push_and_echo_is_suppressed() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        let bytes: Vec<u8> = (0..64).map(|i| (i % 251) as u8).collect();
        let hash = sha256_hex(&bytes);
        cb.simulate_external_image(8, 8, bytes.clone());
        let img = cb.get_image().unwrap();
        let a = e.on_image_poll(Some(img));
        match &a {
            SyncAction::Push(p) => {
                assert_eq!(p.r#type, ProfileType::Image);
                assert_eq!(p.hash, hash);
                assert_eq!(p.width, Some(8));
                assert_eq!(p.height, Some(8));
                assert_eq!(p.data.as_deref(), Some(bytes.as_slice()));
            }
            other => panic!("expected image push, got {other:?}"),
        }
        // 回声：远端把刚推送的图片广播回来 → 忽略
        let pushed = Profile::from_image(bytes, 8, 8, Some("dev-a".to_string()));
        assert_eq!(
            e.apply_remote(&pushed),
            SyncAction::None,
            "image echo must be ignored"
        );
    }

    #[test]
    fn remote_image_writes_local_and_suppresses_echo() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        let bytes: Vec<u8> = (0..64).map(|i| (i % 251) as u8).collect();
        let remote = Profile::from_image(bytes.clone(), 8, 8, Some("dev-b".to_string()));
        let a = e.apply_remote(&remote);
        match &a {
            SyncAction::WriteImage {
                width,
                height,
                data,
                source,
            } => {
                assert_eq!(*width, 8);
                assert_eq!(*height, 8);
                assert_eq!(data, &bytes);
                assert_eq!(source.as_deref(), Some("dev-b"));
            }
            other => panic!("expected image write, got {other:?}"),
        }
        // 执行写本地
        if let SyncAction::WriteImage {
            width,
            height,
            data,
            source,
        } = a
        {
            e.clipboard.set_image(&ImageData {
                width,
                height,
                bytes: data,
            });
            assert_eq!(source.as_deref(), Some("dev-b"));
        }
        let written = cb.get_image().unwrap();
        assert_eq!(written.width, 8);
        assert_eq!(written.height, 8);
        assert_eq!(written.bytes, bytes);
        // 自写抑制：本地轮询到同一图片不应再推送
        let a2 = e.on_image_poll(Some(cb.get_image().unwrap()));
        assert_eq!(a2, SyncAction::None);
    }

    #[test]
    fn image_over_size_limit_is_ignored() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.max_file_byte = 10;
        cb.simulate_external_image(8, 8, vec![0u8; 64]);
        assert_eq!(e.on_image_poll(cb.get_image()), SyncAction::None);
    }

    #[test]
    fn upload_image_disabled_suppresses_image_push() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.upload_image = false;
        cb.simulate_external_image(8, 8, vec![0u8; 64]);
        assert_eq!(e.on_image_poll(cb.get_image()), SyncAction::None);
    }

    #[test]
    fn remote_image_without_data_is_ignored() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, _cb) = engine_with_transport("dev-a", t.clone());
        let remote = Profile::from_image(vec![1u8; 4], 2, 2, Some("dev-b".to_string()));
        let mut broken = remote.clone();
        broken.data = None;
        assert_eq!(e.apply_remote(&broken), SyncAction::None);
    }

    #[tokio::test]
    async fn image_download_notification_carries_source() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = true;
        e.sync.notify_on_downloaded = true;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = seen.clone();
        e.set_download_handler(Arc::new(move |src: Option<String>| {
            seen_cb.lock().unwrap().push(src);
        }));
        let remote = Profile::from_image(vec![7u8; 16], 4, 4, Some("dev-b".to_string()));
        let a = e.apply_remote(&remote);
        e.execute(a).await;
        let written = cb.get_image().unwrap();
        assert_eq!(written.bytes, vec![7u8; 16]);
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            &[Some("dev-b".to_string())]
        );
    }

    #[test]
    fn image_poll_respects_master_and_push_switch() {
        let t = Arc::new(MockTransport::new(None));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.sync.switch_on = false;
        cb.simulate_external_image(2, 2, vec![0u8; 16]);
        assert_eq!(e.on_image_poll(cb.get_image()), SyncAction::None);
        e.sync.switch_on = true;
        e.sync.push_switch_on = false;
        assert_eq!(e.on_image_poll(cb.get_image()), SyncAction::None);
    }

    #[tokio::test]
    async fn startup_pull_executes_adoption_action() {
        // 回归：启动采纳只做决策未执行 → 文本/图片都没有真正写入本地剪贴板
        let bytes: Vec<u8> = (0..64).map(|i| (i % 251) as u8).collect();
        let remote = Profile::from_image(bytes.clone(), 8, 8, Some("dev-b".to_string()));
        let t = Arc::new(MockTransport::new(Some(remote)));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.run_startup_pull().await;
        // 启动采纳必须执行 write：本地剪贴板写入远端图片
        let written = cb.get_image().expect("startup adoption must write image");
        assert_eq!(written.bytes, bytes);
        assert_eq!(written.width, 8);
        assert_eq!(written.height, 8);
        // 写入后自写抑制：同一内容不再推送
        assert_eq!(
            e.on_image_poll(Some(cb.get_image().unwrap())),
            SyncAction::None
        );
    }

    #[tokio::test]
    async fn startup_pull_executes_text_adoption() {
        // 文本启动采纳同样执行写剪贴板
        let remote = Profile::from_text("server-text", Some("dev-b".to_string()));
        let t = Arc::new(MockTransport::new(Some(remote)));
        let (mut e, cb) = engine_with_transport("dev-a", t.clone());
        e.run_startup_pull().await;
        assert_eq!(cb.get().as_deref(), Some("server-text"));
    }
}
