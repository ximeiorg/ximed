//! client 核心库：剪贴板适配 + 同步引擎 + 传输层。
//!
//! 无 GUI 依赖，供 `app` crate 的三种形态（server / client / desktop）复用。
//! `ximed desktop` 与本 crate 同进程共存，`ximed client` 单独运行。

pub mod channel;
pub mod clipboard;
pub mod config;
pub mod history;
pub mod history_store;
/// 局域网 P2P 设备层（feature `p2p`）：节点与配对身份的进程级生命周期。
#[cfg(feature = "p2p")]
pub mod p2p;
#[cfg(feature = "plugin")]
pub mod plugin;
pub mod settings;
pub mod sync;
pub mod transport;
pub mod webdav;
