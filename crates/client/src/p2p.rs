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
//! # 设备表归宿主（协议库 db9dbc8 起）
//!
//! 协议库只定义 [`PeerBook`] 接口，「存在哪、什么格式、要不要迁移」全归宿主。
//! 这里用 `<base>/p2p/peers.json`（[`FilePeerBook`]）——格式与旧版协议库内置
//! 的存储**完全一致**（`{version, peers[]}`，临时文件 + rename 原子替换），
//! 老用户升级后已配对设备原样保留，不需要迁移。
//!
//! 每次查询都读文件、不缓存：「配对线程写的表」和「常驻节点持有的表」必须是
//! 同一份，各缓存一份就会变成「设置里配对成功、同步却说没配对」。写入用进程内
//! 一把全局锁串行化，避免两个写入方各自整份写回、后写的用旧快照抹掉先写的那台。
//!
//! # 配对不再与节点抢端口
//!
//! 同步走 TCP 47821 + UDP 47822，配对走 TCP 47823 + UDP 47824（上游把两条
//! 通道拆开，见 `PAIR_PORT` / `PAIR_UDP_PORT`）。所以配对期间**不需要暂停
//! 节点**——旧版「配对前 shutdown、结束后恢复」的绕过方案已删除，配对时同步
//! 不再中断。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use xime_p2p::{Event, Node, NodeConfig, PairedPeer, PeerBook, PeerState, Profile};

/// 设备层数据目录名（相对宿主数据根）。
const P2P_DIR: &str = "p2p";
/// 已配对设备表文件名（协议 §6.2）。
const PEERS_FILE: &str = "peers.json";
/// 设备表文件格式版本（与旧版协议库内置存储一致）。
const STORE_VERSION: u32 = 1;

/// 设备表写锁。
///
/// 节点的重连学地址（[`PeerBook::set_last_addr`]）与配对线程写新设备可能同时
/// 发生；两方各自「读全表 → 改内存 → 整份写回」会互相覆盖，后写的一方用旧快照
/// 抹掉先写的那台设备。串行化之后每次写都基于最新快照。
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// 进程级节点槽位。
static LINK: OnceLock<Mutex<Option<Node>>> = OnceLock::new();
/// 当前是否有一条已建立的会话。
///
/// 由 [`poll`] / [`wait_event`] 从 `Event::Peer` 维护：`Node` 也提供
/// [`Node::status`] 快照，但引擎每轮都要问「能不能推」，走一个原子读比重入
/// 节点锁便宜。它决定 UI 显示「已连接」还是「等待设备」，也决定 push 是否该
/// 报错（引擎据此退避与上报离线）。
static CONNECTED: AtomicBool = AtomicBool::new(false);

fn slot() -> &'static Mutex<Option<Node>> {
    LINK.get_or_init(|| Mutex::new(None))
}

/// 设备层数据目录（`<base>/p2p`）。
pub fn data_dir(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join(P2P_DIR)
}

// ---------------------------------------------------------------------------
// 设备表：文件版 PeerBook
// ---------------------------------------------------------------------------

/// 磁盘格式（与旧版协议库 `PeerStore` 一致，便于老用户平滑升级）。
#[derive(serde::Serialize, serde::Deserialize)]
struct StoreFile {
    version: u32,
    #[serde(default)]
    peers: Vec<PairedPeer>,
}

/// 文件版 [`PeerBook`]：读就是读文件，写是「读 → 改 → 原子替换」。
///
/// 协议层会对同一个实例从多个任务并发查询/更新，所以所有方法都取 `&self`、
/// 内部自己加锁；不缓存任何状态，跨实例（节点 / 配对会话 / UI）看到的永远是
/// 同一份文件内容。
pub struct FilePeerBook {
    path: PathBuf,
}

impl FilePeerBook {
    /// 指向指定设备表文件。
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 指向设备层数据目录下的标准设备表。
    pub fn at(dir: impl AsRef<Path>) -> Self {
        Self::new(dir.as_ref().join(PEERS_FILE))
    }

    /// 读全表。
    ///
    /// 文件不存在视为空表；文件存在但内容非法则返回错误，**不回落成空表**：
    /// 表里是用户已建立的信任关系，静默清空等于把所有设备退回未配对状态，
    /// 而界面上只会显示「暂无已配对设备」。
    pub fn all_peers(&self) -> Result<Vec<PairedPeer>, String> {
        let raw = match std::fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        let file: StoreFile = serde_json::from_str(&raw).map_err(|e| {
            format!(
                "已配对表解析失败（{}）：{e}。文件未改动，请修复或删除后重新配对",
                self.path.display()
            )
        })?;
        if file.version > STORE_VERSION {
            return Err(format!(
                "已配对表版本 {} 高于本程序支持的 {STORE_VERSION}",
                file.version
            ));
        }
        Ok(file.peers)
    }

    /// 原子写入：写临时文件 → 落盘 → rename 覆盖。
    ///
    /// 中途断电只会丢失本次更新，不会留下半个文件把整张表读废。
    fn save(&self, peers: Vec<PairedPeer>) -> Result<(), String> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let json = serde_json::to_vec_pretty(&StoreFile {
            version: STORE_VERSION,
            peers,
        })
        .map_err(|e| format!("序列化已配对表失败：{e}"))?;
        let tmp = self.path.with_extension("json.tmp");
        {
            use std::io::Write;
            let mut f =
                std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // 表里是设备公钥与地址，不是秘密，但没必要让同机其他用户读到。
                let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            }
            f.write_all(&json)
                .map_err(|e| format!("{}: {e}", tmp.display()))?;
            f.sync_all()
                .map_err(|e| format!("{}: {e}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok(())
    }

    /// 在写锁内「读 → 改 → 写」，保证并发写不互相覆盖。
    ///
    /// 闭包返回 `Err` 时不落盘（例如给未配对的设备写地址），避免把一次失败的
    /// 操作变成一次无意义的重写。
    fn mutate_try<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<String, PairedPeer>) -> Result<T, xime_p2p::Error>,
    ) -> Result<T, xime_p2p::Error> {
        let _guard = WRITE_LOCK
            .lock()
            .map_err(|_| xime_p2p::Error::Store("设备表写锁不可用".into()))?;
        let mut map: BTreeMap<String, PairedPeer> = self
            .all_peers()
            .map_err(xime_p2p::Error::Store)?
            .into_iter()
            .map(|peer| (peer.did.clone(), peer))
            .collect();
        let out = f(&mut map)?;
        self.save(map.into_values().collect())
            .map_err(xime_p2p::Error::Store)?;
        Ok(out)
    }

    /// [`Self::mutate_try`] 的不会失败版本。
    fn mutate<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<String, PairedPeer>) -> T,
    ) -> Result<T, xime_p2p::Error> {
        self.mutate_try(|map| Ok(f(map)))
    }
}

impl PeerBook for FilePeerBook {
    fn get(&self, did: &str) -> Option<PairedPeer> {
        self.all_peers().ok()?.into_iter().find(|p| p.did == did)
    }

    fn by_x25519(&self, pub_x25519: &[u8; 32]) -> Option<PairedPeer> {
        let want = hex::encode(pub_x25519);
        self.all_peers()
            .ok()?
            .into_iter()
            .find(|p| p.pub_x25519 == want)
    }

    fn all(&self) -> Vec<PairedPeer> {
        // 接口没有错误通道；读失败时返回空表不会删文件，界面上的提示由
        // `peers()` 负责（它保留错误，不把损坏的表显示成空表）。
        self.all_peers().unwrap_or_default()
    }

    fn upsert(&self, peer: PairedPeer) -> Result<(), xime_p2p::Error> {
        self.mutate(move |map| {
            let old = map.get(&peer.did).cloned();
            let merged = match old {
                // 重配对（换名字/换公钥）不该把运行时学到的东西清掉。
                Some(old) => PairedPeer {
                    did: peer.did.clone(),
                    name: peer.name,
                    pub_ed: peer.pub_ed,
                    pub_x25519: peer.pub_x25519,
                    last_addr: peer.last_addr.or_else(|| old.last_addr.clone()),
                    last_seen: peer.last_seen.or(old.last_seen),
                    paired_at: peer.paired_at,
                },
                None => peer,
            };
            map.insert(merged.did.clone(), merged);
        })
    }

    fn remove(&self, did: &str) -> Result<bool, xime_p2p::Error> {
        self.mutate(|map| map.remove(did).is_some())
    }

    fn set_last_addr(&self, did: &str, addr: &str) -> Result<(), xime_p2p::Error> {
        self.mutate_try(|map| match map.get_mut(did) {
            Some(peer) => {
                peer.last_addr = Some(addr.to_string());
                Ok(())
            }
            None => Err(xime_p2p::Error::Store(format!("未找到已配对设备 {did}"))),
        })
    }

    fn set_last_seen(&self, did: &str, at: i64) -> Result<(), xime_p2p::Error> {
        self.mutate_try(|map| match map.get_mut(did) {
            Some(peer) => {
                // 只前进不后退：两个任务并发上报时不要把时间戳写回过去。
                if peer.last_seen.is_none_or(|old| at > old) {
                    peer.last_seen = Some(at);
                }
                Ok(())
            }
            None => Err(xime_p2p::Error::Store(format!("未找到已配对设备 {did}"))),
        })
    }
}

// ---------------------------------------------------------------------------
// 节点生命周期
// ---------------------------------------------------------------------------

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
    // 存储归宿主：节点、配对会话、界面各持一个实例，读的都是同一个文件。
    let peers: Arc<dyn PeerBook> = Arc::new(FilePeerBook::at(&dir));
    let cfg = NodeConfig {
        port,
        discover,
        ..NodeConfig::new(identity, peers)
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

/// 节点状态快照（水平值，不是事件）；节点未启动返回 `None`。
fn node_status() -> Option<xime_p2p::NodeStatus> {
    with(|node| node.status())
}

// ---------------------------------------------------------------------------
// 已配对设备
// ---------------------------------------------------------------------------

/// 设备在场状态（界面用）。
///
/// 不直接暴露协议类型，免得 app 再去依赖 `xime-p2p`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// 有活跃会话，能立刻同步。
    Online,
    /// 最近收到过公告，但没有会话（对端在跑，只是没连上本机）。
    Nearby,
    /// 既没会话也没公告。
    Offline,
    /// 节点未启动，拿不到即时状态。
    Unknown,
}

impl Presence {
    fn of(state: PeerState) -> Self {
        match state {
            PeerState::Online => Presence::Online,
            PeerState::Nearby => Presence::Nearby,
            PeerState::Offline => Presence::Offline,
        }
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
    /// 最后一次确认在线的时刻（Unix 秒）；从没连上过为 `None`。
    ///
    /// 节点刚启动、还没连上任何设备时，界面靠它显示「上次在线 …」而不是
    /// 一句干巴巴的「离线」。
    pub last_seen: Option<i64>,
    /// 即时在场状态（节点没起来时为 [`Presence::Unknown`]）。
    pub presence: Presence,
}

/// 读取已配对设备表（含即时在场状态）。
///
/// 表损坏（文件存在但内容非法）时返回错误，**不回落为空列表**：表里存的是用户
/// 已建立的信任关系，静默清空等于把所有设备退回未配对状态，而界面上只会显示
/// 「暂无已配对设备」。
pub fn peers(base: impl AsRef<Path>) -> Result<Vec<PeerInfo>, String> {
    let book = FilePeerBook::at(data_dir(base));
    let mut list: Vec<PeerInfo> = book
        .all_peers()?
        .into_iter()
        .map(|peer| PeerInfo {
            did: peer.did,
            name: peer.name,
            last_addr: peer.last_addr,
            last_seen: peer.last_seen,
            presence: Presence::Unknown,
        })
        .collect();
    // 即时状态只有活着的节点知道。节点没起来时保持「未知」而不是「离线」——
    // 那会让人以为对端关机了。
    if let Some(status) = node_status() {
        for info in &mut list {
            if let Some(hit) = status.peers.iter().find(|p| p.did == info.did) {
                info.presence = Presence::of(hit.state);
                if hit.last_seen.is_some() {
                    info.last_seen = hit.last_seen;
                }
            }
        }
    }
    Ok(list)
}

/// 是否至少有一台已配对设备（没有对端就没必要为同步去动网络）。
pub fn has_peer(base: impl AsRef<Path>) -> bool {
    peers(base).map(|list| !list.is_empty()).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 通道
// ---------------------------------------------------------------------------

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

/// 打开设备层的身份与设备表（配对会话与常驻节点共用同一份）。
fn pairing_context(base: &Path) -> Result<(Arc<xime_p2p::Identity>, Arc<dyn PeerBook>), String> {
    let dir = data_dir(base);
    std::fs::create_dir_all(&dir).map_err(|e| format!("P2P 目录创建失败：{e}"))?;
    let identity = xime_p2p::load_or_create_identity(&dir)
        .ok_or_else(|| "P2P 身份不可用（系统随机源异常）".to_string())?;
    let peers: Arc<dyn PeerBook> = Arc::new(FilePeerBook::at(&dir));
    Ok((identity, peers))
}

/// 开始「显示配对码」：立即返回码，等待与结果在后台线程完成。
///
/// 不再暂停常驻节点：配对用自己的 TCP/UDP 端口，与同步发现互不相干。
/// 回调在后台线程执行，宿主应通过自己的事件通道送回 UI 线程。
pub fn start_display_pairing(
    base: impl AsRef<Path>,
    name: String,
    on_event: impl FnOnce(PairEvent) + Send + 'static,
) -> Result<DisplayPairing, String> {
    let root = base.as_ref().to_path_buf();
    let (identity, peers) = pairing_context(&root)?;
    let mut session = xime_p2p::DisplaySession::start(identity, peers, name)
        .map_err(|e| format!("配对端口不可用：{e}"))?;
    let code = session.code_text();
    let stop = session.stop_handle();
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
    let (identity, peers) = pairing_context(&root)?;
    std::thread::Builder::new()
        .name("xime-p2p-join".to_string())
        .spawn(move || {
            let event = match xime_p2p::join(identity, peers, &code, name, xime_p2p::CODE_TTL) {
                Ok(peer) => PairEvent::Paired {
                    name: peer.name().to_string(),
                    did: peer.did().to_hex(),
                },
                Err(e) => PairEvent::Failed(friendly_error(&e)),
            };
            on_event(event);
        })
        .map_err(|e| format!("配对线程创建失败：{e}"))?;
    Ok(())
}

/// 移除一台已配对设备。移除后对端需要重新配对才能连上（信任是双向的）。
pub fn remove_peer(base: impl AsRef<Path>, did: &str) -> Result<bool, String> {
    let book = FilePeerBook::at(data_dir(base));
    PeerBook::remove(&book, did).map_err(|e| e.to_string())
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

        // 节点在跑时配对必须照样能开始。这是旧实现的痛点：配对与同步共用
        // 端口，必须先 shutdown 节点。上游把两条通道的端口拆开了，这里先把
        // 「确实不共用」钉住，再验证节点活着时配对能正常启动。
        assert_ne!(xime_p2p::PAIR_PORT, xime_p2p::TCP_PORT);
        assert_ne!(xime_p2p::PAIR_UDP_PORT, xime_p2p::UDP_PORT);
        // 放在同一片测试里做是因为节点是进程级单例，拆开并发跑会互相顶掉。
        let pairing = start_display_pairing(base, "测试机".to_string(), |_| {})
            .expect("节点在跑时配对应能开始");
        assert!(!pairing.code().is_empty(), "配对码应立即可用");
        pairing.cancel();

        // 节点在跑时，已配对设备要带即时状态：既没有会话也没有公告 → 离线。
        // 这条路径是设备页那排状态胶囊的数据来源。
        let book = FilePeerBook::at(data_dir(base));
        let other = xime_p2p::Identity::generate().unwrap();
        book.upsert(PairedPeer::from_public(&other.public("另一台"), None))
            .unwrap();
        let list = peers(base).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0].presence,
            Presence::Offline,
            "节点在跑时应给出即时状态，而不是「未知」"
        );

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
        let peer_identity = xime_p2p::Identity::generate().unwrap();
        let book = FilePeerBook::at(data_dir(base));
        book.upsert(PairedPeer::from_public(
            &peer_identity.public("办公室 Mac"),
            Some("192.168.1.5:47821".to_string()),
        ))
        .unwrap();

        let list = peers(base).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "办公室 Mac");
        assert_eq!(list[0].last_addr.as_deref(), Some("192.168.1.5:47821"));
        // 节点没起来时状态是「未知」，不是「离线」。
        assert_eq!(list[0].presence, Presence::Unknown);
        assert!(has_peer(base));
    }

    /// 重配对（改名）不能把运行时学到的地址与在线时刻清掉。
    #[test]
    fn 重新配对保留地址与在线时刻() {
        let dir = tempdir().unwrap();
        let book = FilePeerBook::at(data_dir(dir.path()));
        let identity = xime_p2p::Identity::generate().unwrap();
        let did = identity.did().to_hex();
        book.upsert(PairedPeer::from_public(&identity.public("旧名字"), None))
            .unwrap();
        book.set_last_addr(&did, "10.0.0.2:47821").unwrap();
        book.set_last_seen(&did, 100).unwrap();

        book.upsert(PairedPeer::from_public(&identity.public("新名字"), None))
            .unwrap();

        let got = book.get(&did).expect("设备应仍在表里");
        assert_eq!(got.name, "新名字");
        assert_eq!(got.last_addr.as_deref(), Some("10.0.0.2:47821"));
        assert_eq!(got.last_seen, Some(100));
    }

    /// 同一进程里两个实例（节点 / 配对线程）写同一张表时互不覆盖。
    #[test]
    fn 两个实例写同一张表互不覆盖() {
        let dir = tempdir().unwrap();
        let path = data_dir(dir.path()).join(PEERS_FILE);
        let a = FilePeerBook::new(&path);
        let b = FilePeerBook::new(&path);

        let id_a = xime_p2p::Identity::generate().unwrap();
        let id_b = xime_p2p::Identity::generate().unwrap();
        a.upsert(PairedPeer::from_public(&id_a.public("A"), None))
            .unwrap();
        b.upsert(PairedPeer::from_public(&id_b.public("B"), None))
            .unwrap();

        assert_eq!(a.all().len(), 2, "后写的不能抹掉先写的设备");
        assert!(a.remove(&id_a.did().to_hex()).unwrap());
        assert!(
            b.get(&id_a.did().to_hex()).is_none(),
            "删除对两个实例都生效"
        );
    }

    /// 已配对设备的公钥反查（Noise 握手用）。
    #[test]
    fn 按_x25519_反查设备() {
        let dir = tempdir().unwrap();
        let book = FilePeerBook::at(data_dir(dir.path()));
        let identity = xime_p2p::Identity::generate().unwrap();
        let public = identity.public("Phone");
        let did = public.did().to_hex();
        book.upsert(PairedPeer::from_public(&public, None)).unwrap();

        assert_eq!(book.by_x25519(&public.x_public()).map(|p| p.did), Some(did));
        assert!(book.by_x25519(&[0u8; 32]).is_none());
    }
}
