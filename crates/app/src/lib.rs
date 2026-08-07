//! `ximed` 应用库：三种部署形态的入口与桌面 UI 模块。
//!
//! 三种独立二进制（各自调用这里的一个入口）：
//!
//! ```text
//! ximed-server    # Form 1：仅服务器（RPi/云服务器部署，无 UI）
//! ximed-client    # Form 3：仅客户端（tray feature 时带桌面 UI，否则纯 daemon）
//! ximed-desktop   # Form 2：同进程 server + client + 托盘（桌面 UI，需 tray feature）
//! ```
//!
//! 构建：
//! - 默认（`tray` feature）：三个二进制全可用；client / desktop 带 iced 窗口 + 托盘
//! - `--no-default-features`：仅 `ximed-server` + `ximed-client`（纯 daemon 无 UI）；
//!   `ximed-desktop` 因 `required-features` 不参与构建。仅用于 Form 1 精简部署（低性能设备）。

#[cfg(feature = "tray")]
pub mod desktop;
#[cfg(all(feature = "tray", target_os = "macos"))]
pub mod macos;
#[cfg(feature = "tray")]
pub mod settings;
#[cfg(feature = "tray")]
pub mod tray;
#[cfg(feature = "tray")]
pub mod ui;

#[cfg(feature = "tray")]
use std::path::PathBuf;
use std::sync::Arc;

use client::clipboard::SystemClipboard;
use client::config::ClientConfig;
use client::settings::SyncSettings;
use client::sync::SyncEngine;

/// Form 1：仅服务器。行为与原 `ximed server` 子命令一致。
pub fn run_server() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings = server::config::Settings::new()?;
    if let Err(msg) = server::check_auth_password(&settings) {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }
    server::init_tracing(&settings.server().log_level);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(server::run(settings))
}

/// Form 3：仅客户端。连远程服务器同步本地剪贴板。
///
/// 默认（`tray` feature）带 iced 窗口 + 托盘；`--no-default-features` 构建退化为
/// 纯后台 daemon（无 UI）。
pub fn run_client() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cfg = ClientConfig::load()?;
    let password = match cfg.password() {
        Some(p) => p,
        None => {
            eprintln!(
                "error: client password not set; set [client] password in config file, or export {}",
                cfg.password_env
            );
            std::process::exit(1);
        }
    };
    server::init_tracing("info");
    // client-only 形态无设置面板：默认开启同步（产品默认主开关关，但 CLI 形态显式启动即代表启用）
    let sync = SyncSettings {
        switch_on: true,
        ..SyncSettings::default()
    };

    #[cfg(feature = "tray")]
    {
        // 桌面客户端：iced 窗口 + 托盘 + 后台同步引擎 + 历史轮询
        let (tx, rx) = ui::ui_channel();
        let (cmd_tx, _cmd_rx) = ui::ui_cmd_channel();
        let ui_cfg = cfg.clone();
        let ui_tx = tx.clone();
        let ui_cmd_tx = cmd_tx.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("failed to build runtime: {e}");
                    return;
                }
            };
            rt.block_on(async {
                let cb = Arc::new(SystemClipboard::new().expect("failed to init clipboard"));
                let mut engine = SyncEngine::new(cfg.clone(), sync.clone(), password.clone(), cb);
                // 本地历史：引擎记录 + 历史面板读取（client-only 无 server，纯本地）
                let local_history = client::history_store::LocalHistory::open(
                    PathBuf::from("client-history.db"),
                    100,
                )
                .unwrap_or_else(|| {
                    client::history_store::LocalHistory::open(
                        std::env::temp_dir().join("ximed-client-history.db"),
                        100,
                    )
                    .expect("local history must open in temp dir as fallback")
                });
                let history_for_engine = local_history.clone();
                engine.set_history_handler(Arc::new(move |profile: &domain::profile::Profile| {
                    history_for_engine.record(profile);
                }));
                let tx_engine = tx.clone();
                let source = cfg.source.clone();
                let _ = tx_engine.send(ui::UiMsg::EngineStarted {
                    source: source.clone(),
                });
                let engine_task = tokio::spawn(async move {
                    engine.run().await;
                    let _ = tx_engine.send(ui::UiMsg::EngineStopped { source });
                });
                // 历史面板轮询：统一读取客户端本地历史
                let history = local_history;
                let tx_history = tx.clone();
                let history_task = tokio::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
                    loop {
                        interval.tick().await;
                        let items = history.list(100);
                        let _ = tx_history.send(ui::UiMsg::History { items });
                    }
                });
                tokio::select! {
                    _ = engine_task => {}
                    _ = history_task => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            });
        });
        // client-only：无本地 server，不显示服务器控制区与设置面板
        ui::run_ui(
            ui_cfg,
            settings::AppSettings::default(),
            ui_tx,
            ui_cmd_tx,
            rx,
            false,
            String::new(),
        )
        .map_err(|e| e.into())
    }

    #[cfg(not(feature = "tray"))]
    {
        // 精简构建：纯后台 daemon（无托盘）
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let clipboard = Arc::new(SystemClipboard::new()?);
            let mut engine = SyncEngine::new(cfg, sync, password, clipboard);
            engine.run().await;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })
    }
}

/// Form 2：桌面形态。同进程启动 server + client 引擎。
#[cfg(feature = "tray")]
pub fn run_desktop() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    desktop::run()
}
