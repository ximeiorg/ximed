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
