//! 局域网 P2P 设备层：节点（监听 / 发现 / 会话）的进程级生命周期。
//!
//! 分层理由：**配对是认证，同步是功能**。节点属于设备层，剪贴板同步只是它的
//! 订阅者——以后加词库、主题同步时节点不重建、不抢端口。与 XimeYi
//! `device_link.rs` 同构。
//!
//! 单例化的直接原因：desktop 每次「保存并应用」都会 abort 引擎任务再重建
//! [`crate::sync::SyncEngine`]（`app/src/desktop.rs`）。节点若跟着引擎走，改一次
//! 设置就重建一次节点：重新抢端口、丢掉已建立的连接，表现为「点了保存，同步断
//! 一会」。所以节点第一次被需要时启动，之后跨通道切换存活，只有进程退出才停。
//!
//! # 线程约束（宿主必须遵守）
//!
//! [`ensure`] 与 [`wait_event`] 内部都会**阻塞**（`Node` 自建 runtime 并
//! `block_on`）。因此：
//!
//! - 不要在 tokio 上下文里直接调用，否则 `block_on` 会 panic；用
//!   `tokio::task::spawn_blocking` 或独立线程。
//! - 单例在进程存活期间 **不 drop**：`Node::drop` 会 `shutdown_timeout` 阻塞，
//!   在异步任务里析构同样会 panic。
//!
//! 数据目录固定为 `<base>/p2p`（`identity.bin` + `peers.json`），配对入口与常驻
//! 节点必须指向同一目录——各写各的会得到「设置里配对成功、输入法仍认为未配对」，
//! 而界面上看不出任何异常。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use xime_p2p::{Event, Node, NodeConfig, PeerStore, Profile};

/// 设备层数据目录名（相对宿主数据根）。
const P2P_DIR: &str = "p2p";
/// 已配对设备表文件名（协议 §6.2）。
const PEERS_FILE: &str = "peers.json";

/// 进程级节点槽位。
static LINK: OnceLock<Mutex<Option<Node>>> = OnceLock::new();
/// 当前是否有一条已建立的会话。
///
/// 由 [`poll`] / [`wait_event`] 从 `Event::Peer` 维护：`Node` 本身不暴露连接态，
/// 而「有没有对端」直接决定 UI 该显示「已连接」还是「等待设备」，也决定 push
/// 是否该报错（引擎据此退避与上报离线）。
static CONNECTED: AtomicBool = AtomicBool::new(false);

fn slot() -> &'static Mutex<Option<Node>> {
    LINK.get_or_init(|| Mutex::new(None))
}

/// 设备层数据目录（`<base>/p2p`）。
pub fn data_dir(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join(P2P_DIR)
}

/// 确保节点已启动，返回实际监听端口。
///
/// 幂等：已启动直接返回当前端口。端口在返回前已绑定完成，因此端口号可以立刻
/// 展示给用户。启动失败（目录不可写 / 无随机源 / 端口耗尽）返回可读错误，调用方
/// 据此降级到其它通道。
///
/// 端口被占用会自动顺延，**不要假定等于 [`xime_p2p::TCP_PORT`]**。
pub fn ensure(base: impl AsRef<Path>) -> Result<u16, String> {
    start(base.as_ref(), xime_p2p::TCP_PORT, true)
}

fn start(base: &Path, port: u16, discover: bool) -> Result<u16, String> {
    let mut guard = slot()
        .lock()
        .map_err(|_| "设备层状态锁已损坏".to_string())?;
    if let Some(node) = guard.as_ref() {
        return Ok(node.local_port());
    }
    let dir = data_dir(base);
    std::fs::create_dir_all(&dir).map_err(|e| format!("P2P 目录创建失败：{e}"))?;
    let identity = xime_p2p::load_or_create_identity(&dir)
        .ok_or_else(|| "P2P 身份不可用（系统随机源异常）".to_string())?;
    let cfg = NodeConfig {
        port,
        discover,
        ..NodeConfig::new(identity, dir.join(PEERS_FILE))
    };
    let node = Node::start(cfg).map_err(|e| format!("P2P 节点启动失败：{e}"))?;
    let actual = node.local_port();
    tracing::info!(port = actual, dir = %dir.display(), "p2p: 设备层节点已启动");
    *guard = Some(node);
    Ok(actual)
}

/// 在持有节点时执行一段操作；节点未启动返回 `None`。
pub fn with<R>(f: impl FnOnce(&Node) -> R) -> Option<R> {
    slot().lock().ok()?.as_ref().map(f)
}

/// 节点是否已启动。
pub fn is_running() -> bool {
    slot().lock().map(|guard| guard.is_some()).unwrap_or(false)
}

/// 本机设备标识（32 位 hex）；节点未启动返回 `None`。
pub fn local_did() -> Option<String> {
    with(|node| node.did().to_string())
}

/// 当前是否有活跃连接（由事件流维护）。
pub fn is_connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

/// 投递一条剪贴板条目。
///
/// 无连接时不会报错：节点会缓存最近一条，等对端连上后补发（协议只关心最新值）。
/// 因此这里的 `false` 只表示「节点没起来」，不代表「对端不可达」。
pub fn set_clipboard(profile: Profile) -> bool {
    with(|node| node.set_clipboard(profile)).unwrap_or(false)
}

/// 非阻塞取一个事件；同时把连接状态记到 [`is_connected`]。
pub fn poll() -> Option<Event> {
    let event = with(|node| node.poll_event()).flatten()?;
    note(&event);
    Some(event)
}

/// 等待一个事件，最多等 `timeout`。**阻塞调用**，见模块文档的线程约束。
pub fn wait_event(timeout: Duration) -> Option<Event> {
    let event = with(|node| node.wait_event(timeout)).flatten()?;
    note(&event);
    Some(event)
}

fn note(event: &Event) {
    if let Event::Peer { connected, .. } = event {
        CONNECTED.store(*connected, Ordering::Relaxed);
    }
}

/// 停止节点（仅进程退出路径使用；重启后需要重新 [`ensure`]）。
pub fn shutdown() {
    CONNECTED.store(false, Ordering::Relaxed);
    let Ok(mut guard) = slot().lock() else { return };
    if let Some(node) = guard.take() {
        node.shutdown();
        tracing::info!("p2p: 设备层节点已停止");
    }
}

/// 已配对设备（供 UI 展示与移除）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// 设备标识（32 位 hex）。
    pub did: String,
    /// 对端自报的设备名，仅用于展示。
    pub name: String,
    /// 最近一次成功连接的地址，尚未连接过为 `None`。
    pub last_addr: Option<String>,
}

/// 读取已配对设备表。
///
/// 表损坏（文件存在但内容非法）时返回错误，**不回落为空列表**：表里存的是用户
/// 已建立的信任关系，静默清空等于把所有设备退回未配对状态，而界面上只会显示
/// 「暂无已配对设备」。
pub fn peers(base: impl AsRef<Path>) -> Result<Vec<PeerInfo>, String> {
    let store = PeerStore::load(data_dir(base).join(PEERS_FILE)).map_err(|e| e.to_string())?;
    Ok(store
        .all()
        .map(|peer| PeerInfo {
            did: peer.did.clone(),
            name: peer.name.clone(),
            last_addr: peer.last_addr.clone(),
        })
        .collect())
}

/// 是否至少有一台已配对设备（没有对端就没必要为同步去动网络）。
pub fn has_peer(base: impl AsRef<Path>) -> bool {
    peers(base).map(|list| !list.is_empty()).unwrap_or(false)
}

/// 启动设备层并包装成同步通道。
///
/// # 为什么构造要另起线程
///
/// [`ensure`] 内部 `block_on`，在 tokio 上下文里直接调用会 panic，而通道是在
/// `SyncEngine::new`（异步上下文）里装配的。这里把启动丢到独立线程并等结果，
/// 最多等 [`START_TIMEOUT`]；超时按启动失败处理，调用方降级到备用通道。
pub fn open_transport(base: impl AsRef<Path>) -> Result<P2pTransport, String> {
    let base = base.as_ref().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("xime-p2p-start".to_string())
        .spawn(move || {
            let _ = tx.send(ensure(&base));
        })
        .map_err(|e| format!("P2P 启动线程创建失败：{e}"))?;
    match rx.recv_timeout(START_TIMEOUT) {
        Ok(Ok(port)) => Ok(P2pTransport { port }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("P2P 节点启动超时".to_string()),
    }
}

/// 节点启动的最长等待时间（绑定端口 + 读身份，正常在毫秒级）。
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// 局域网直连通道。
///
/// 语义与 HTTP / WebDAV 的差异（必须显式处理，否则界面会撒谎）：
///
/// - **推模型、无快照**：没有「服务器当前值」可拉，[`Transport::pull`] 恒为 `None`；
/// - **无对端不等于成功**：`set_clipboard` 在没有对端时只是缓存，直接返回 `Ok`
///   会让引擎一直上报「在线」。这里在无连接时返错，复用引擎既有的退避与离线
///   上报——用户看到的「离线」才是真实状态。
pub struct P2pTransport {
    /// 实际监听端口（可能因占用顺延，仅用于日志与展示）。
    port: u16,
}

impl P2pTransport {
    /// 本机实际监听端口。
    pub fn port(&self) -> u16 {
        self.port
    }
}

#[async_trait::async_trait]
impl crate::transport::Transport for P2pTransport {
    async fn push(&self, profile: &Profile) -> Result<(), crate::transport::TransportError> {
        if !is_connected() {
            return Err(crate::transport::TransportError::Connect(
                "局域网内没有在线设备".to_string(),
            ));
        }
        if set_clipboard(profile.clone()) {
            Ok(())
        } else {
            Err(crate::transport::TransportError::Send(
                "P2P 节点未运行".to_string(),
            ))
        }
    }

    async fn pull(&self) -> Result<Option<Profile>, crate::transport::TransportError> {
        // 推模型没有快照可拉；启动补发由节点缓存负责（协议只关心最新值）。
        Ok(None)
    }

    async fn next_event(
        &self,
        timeout: Duration,
    ) -> Result<Option<Profile>, crate::transport::TransportError> {
        // 用非阻塞 poll 轮询而不是 spawn_blocking(wait_event)：引擎每个轮询周期都会
        // 调一次，spawn_blocking 会持续占用阻塞线程池；50ms 粒度对剪贴板足够。
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match poll() {
                Some(Event::Clipboard(profile)) => return Ok(Some(profile)),
                Some(_) => continue,
                None => {}
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(POLL_STEP).await;
        }
    }
}

/// 事件轮询步长。
const POLL_STEP: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// 配对
// ---------------------------------------------------------------------------

/// 配对结果（后台线程 → UI）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairEvent {
    /// 配对成功，对端已写入设备表。
    Paired { name: String, did: String },
    /// 失败原因（已转成给人看的中文描述）。
    Failed(String),
}

/// 显示端配对会话句柄。
///
/// 会话本体被等待线程独占（`wait` 会阻塞），UI 只留这个取消句柄与配对码。
/// `Clone` 只克隆码与取消句柄（`Arc` 内部），不会复制会话或再开端口。
#[derive(Clone)]
pub struct DisplayPairing {
    code: String,
    stop: xime_p2p::StopHandle,
    /// 生码时刻：界面据此显示倒计时（码本身 60 秒过期）。
    issued: std::time::Instant,
}

impl std::fmt::Debug for DisplayPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DisplayPairing")
            .field("code", &self.code)
            .finish_non_exhaustive()
    }
}

impl DisplayPairing {
    /// 显示给用户的码，形如 `482 719`。
    pub fn code(&self) -> &str {
        &self.code
    }

    /// 叫停等待：用于「重新生成」与「取消」。不叫停会一直占着配对端口。
    pub fn cancel(&self) {
        self.stop.cancel();
    }

    /// 剩余有效期秒数（过期返回 0）。
    pub fn remaining_secs(&self) -> u64 {
        xime_p2p::CODE_TTL
            .saturating_sub(self.issued.elapsed())
            .as_secs()
    }

    /// 剩余比例（1.0 → 0.0），用于倒计时进度条。
    pub fn remaining_ratio(&self) -> f32 {
        let total = xime_p2p::CODE_TTL.as_secs_f32().max(1.0);
        (self.remaining_secs() as f32 / total).clamp(0.0, 1.0)
    }
}

/// 暂停常驻节点，为配对让出端口。
///
/// 配对与常驻节点抢同一组端口（节点 TCP `47821` + 发现 UDP `47822`，配对监听
/// TCP `47822` + 广播 UDP `47822`），而这对监听都没有 `SO_REUSEADDR`。桌面是单
/// 进程，不暂停就 100% 绑不上——表现为「点配对没反应」，日志里只有一行 EADDRINUSE。
fn pause_node() -> bool {
    let was_running = is_running();
    if was_running {
        shutdown();
    }
    was_running
}

/// 配对结束后恢复节点（原本没起就不要凭空启动）。
fn resume_node(base: &Path, was_running: bool) {
    if !was_running {
        return;
    }
    if let Err(e) = ensure(base) {
        tracing::warn!("配对结束后重启 P2P 节点失败：{e}");
    }
}

/// 开始「显示配对码」：立即返回码，等待与结果在后台线程完成。
///
/// 回调在后台线程执行，宿主应通过自己的事件通道送回 UI 线程。
pub fn start_display_pairing(
    base: impl AsRef<Path>,
    name: String,
    on_event: impl FnOnce(PairEvent) + Send + 'static,
) -> Result<DisplayPairing, String> {
    let root = base.as_ref().to_path_buf();
    let dir = data_dir(&root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("P2P 目录创建失败：{e}"))?;
    let was_running = pause_node();
    let identity = xime_p2p::load_or_create_identity(&dir)
        .ok_or_else(|| "P2P 身份不可用（系统随机源异常）".to_string())?;
    let store_path = dir.join(PEERS_FILE);
    let session = match xime_p2p::DisplaySession::start(identity, store_path, name) {
        Ok(s) => s,
        Err(e) => {
            resume_node(&root, was_running);
            return Err(format!("配对端口不可用：{e}"));
        }
    };
    let code = session.code_text();
    let stop = session.stop_handle();
    let mut session = session;
    std::thread::Builder::new()
        .name("xime-p2p-pair".to_string())
        .spawn(move || {
            let event = match session.wait(xime_p2p::CODE_TTL) {
                Ok(peer) => PairEvent::Paired {
                    name: peer.name().to_string(),
                    did: peer.did().to_hex(),
                },
                Err(e) => PairEvent::Failed(friendly_error(&e)),
            };
            resume_node(&root, was_running);
            on_event(event);
        })
        .map_err(|e| format!("配对线程创建失败：{e}"))?;
    Ok(DisplayPairing {
        code,
        stop,
        issued: std::time::Instant::now(),
    })
}

/// 输入端：用对方的码配对（后台线程，立即返回）。
pub fn start_join_pairing(
    base: impl AsRef<Path>,
    name: String,
    code: String,
    on_event: impl FnOnce(PairEvent) + Send + 'static,
) -> Result<(), String> {
    let root = base.as_ref().to_path_buf();
    let dir = data_dir(&root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("P2P 目录创建失败：{e}"))?;
    let was_running = pause_node();
    let identity = xime_p2p::load_or_create_identity(&dir)
        .ok_or_else(|| "P2P 身份不可用（系统随机源异常）".to_string())?;
    let store_path = dir.join(PEERS_FILE);
    std::thread::Builder::new()
        .name("xime-p2p-join".to_string())
        .spawn(move || {
            let event = match xime_p2p::join(identity, store_path, &code, name, xime_p2p::CODE_TTL)
            {
                Ok(peer) => PairEvent::Paired {
                    name: peer.name().to_string(),
                    did: peer.did().to_hex(),
                },
                Err(e) => PairEvent::Failed(friendly_error(&e)),
            };
            resume_node(&root, was_running);
            on_event(event);
        })
        .map_err(|e| format!("配对线程创建失败：{e}"))?;
    Ok(())
}

/// 移除一台已配对设备。移除后对端需要重新配对才能连上（信任是双向的）。
pub fn remove_peer(base: impl AsRef<Path>, did: &str) -> Result<bool, String> {
    let path = data_dir(base.as_ref()).join(PEERS_FILE);
    let mut store = PeerStore::load(&path).map_err(|e| e.to_string())?;
    let removed = store.remove(did).map_err(|e| e.to_string())?;
    store.save().map_err(|e| e.to_string())?;
    Ok(removed)
}

/// 节点实际监听端口（未启动返回 `None`）。
pub fn local_port() -> Option<u16> {
    with(|node| node.local_port())
}

/// 把协议错误翻成用户能看懂的一句话。
///
/// 直接把 `Error` 的 Debug 丢到界面上，用户只会看到 `AuthFailed` 这种词——而
/// 配对失败最常见的原因其实是「码输错了」或「对方没在配对界面」。
fn friendly_error(err: &xime_p2p::Error) -> String {
    use xime_p2p::Error;
    match err {
        Error::AuthFailed => "配对码不正确，请核对后重试".to_string(),
        Error::CodeExpired => "配对码已过期，请在对方设备上重新生成".to_string(),
        Error::TooManyAttempts => "尝试次数过多，请重新生成配对码".to_string(),
        Error::Busy => "对方正在与另一台设备配对".to_string(),
        Error::VersionMismatch { .. } => "两台设备的协议版本不一致，请升级后重试".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// 节点启动、端口可读、重复调用幂等、停止后状态一致。
    ///
    /// 单片测试覆盖全部单例语义：节点是进程级单例，多个测试并发跑会互相顶掉。
    #[test]
    fn 节点幂等启动且端口可读() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        // 端口用 0 取临时端口、关掉发现：测试不依赖广播，也不去抢 47822。
        let port = start(base, 0, false).expect("节点应能启动");
        assert_ne!(port, 0, "实际监听端口必须可读");
        assert!(is_running());
        assert_eq!(
            start(base, 0, false).expect("重复调用应幂等"),
            port,
            "已启动时不得换端口"
        );
        assert_eq!(local_did().map(|d| d.len()), Some(32), "did 应为 32 位 hex");

        // 无对端时投递不阻塞、不报错（节点会缓存，等连上补发）。
        assert!(set_clipboard(Profile::from_text("hello", None)));
        assert!(!is_connected(), "没有对端时不应报告已连接");

        shutdown();
        assert!(!is_running());
        assert!(!is_connected());
        assert!(local_did().is_none());
    }

    #[test]
    fn 空设备表返回空列表() {
        let dir = tempdir().unwrap();
        assert!(peers(dir.path()).unwrap().is_empty());
        assert!(!has_peer(dir.path()));
    }

    /// 配对表损坏必须报错：静默当作空表等于把已配对设备全部作废。
    #[test]
    fn 设备表损坏时返回错误() {
        let dir = tempdir().unwrap();
        let p2p = data_dir(dir.path());
        std::fs::create_dir_all(&p2p).unwrap();
        std::fs::write(p2p.join(PEERS_FILE), "{ not json").unwrap();
        assert!(peers(dir.path()).is_err());
        // 不能因为读失败就当成「没有对端」而悄悄继续
        assert!(!has_peer(dir.path()));
    }

    #[test]
    fn 设备表可读出已配对设备() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let p2p = data_dir(base);
        std::fs::create_dir_all(&p2p).unwrap();
        let peer_identity = xime_p2p::Identity::generate().unwrap();
        let mut store = PeerStore::load(p2p.join(PEERS_FILE)).unwrap();
        store
            .upsert(xime_p2p::PairedPeer::from_public(
                &peer_identity.public("办公室 Mac"),
                Some("192.168.1.5:47821".to_string()),
            ))
            .unwrap();
        store.save().unwrap();

        let list = peers(base).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "办公室 Mac");
        assert_eq!(list[0].last_addr.as_deref(), Some("192.168.1.5:47821"));
        assert!(has_peer(base));
    }
}
