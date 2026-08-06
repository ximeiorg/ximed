//! client 核心库：剪贴板适配 + 同步引擎 + 传输层。
//!
//! 无 GUI 依赖，供 `app` crate 的三种形态（server / client / desktop）复用。
//! `ximed desktop` 与本 crate 同进程共存，`ximed client` 单独运行。

pub mod clipboard;
pub mod config;
pub mod history;
pub mod sync;
pub mod transport;
