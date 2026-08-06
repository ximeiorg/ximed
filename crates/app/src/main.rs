//! `ximed` 统一二进制：三种部署形态。
//!
//! ```text
//! ximed server      # Form 1：仅服务器（RPi/云服务器部署，无 UI）
//! ximed client      # Form 3：仅客户端 daemon + 托盘（连远程服务器，桌面 UI）
//! ximed desktop     # Form 2：同进程 server + client + 托盘（桌面部署，桌面 UI）
//! ```
//!
//! 构建：
//! - 默认（`tray` feature）：三种形态全可用；client / desktop 带 vizia 窗口 + 托盘
//! - `--no-default-features`：server（无 UI）+ client（纯 daemon 无托盘）；
//!   desktop 子命令不可用。仅用于 Form 1 精简部署（低性能设备）。

#[cfg(feature = "tray")]
mod desktop;
#[cfg(feature = "tray")]
mod settings;
#[cfg(feature = "tray")]
mod tray;
#[cfg(feature = "tray")]
mod ui;

use std::sync::Arc;

use clap::{Parser, Subcommand};

use client::clipboard::SystemClipboard;
use client::config::ClientConfig;
use client::sync::SyncEngine;

/// ximed 输入法数据同步服务。
#[derive(Parser)]
#[command(name = "ximed", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 仅启动服务器（Form 1，无 UI）
    Server,
    /// 仅启动客户端 daemon + 托盘（Form 3）
    Client,
    /// 桌面形态：同进程 server + client + 托盘（Form 2，需 tray feature）
    #[cfg(feature = "tray")]
    Desktop,
}

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    match cli.command {
        Command::Server => run_server_only(),
        Command::Client => run_client_only(),
        #[cfg(feature = "tray")]
        Command::Desktop => run_desktop(),
    }
}

/// Form 1：仅服务器。行为与 `server` 二进制一致。
fn run_server_only() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings = server::config::Settings::new()?;
    if let Err(msg) = server::check_auth_password(&settings) {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }
    server::init_tracing(&settings.server().log_level);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(server::run(settings))
}

/// Form 3：仅客户端 daemon + 托盘。连远程服务器同步本地剪贴板。
///
/// 默认（`tray` feature）带 vizia 窗口 + 托盘；`--no-default-features` 构建退化为
/// 纯后台 daemon（无 UI）。
fn run_client_only() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
                let mut engine = SyncEngine::new(cfg.clone(), password.clone(), cb);
                let tx_engine = tx.clone();
                let source = cfg.source.clone();
                let _ = tx_engine.send(ui::UiMsg::EngineStarted { source: source.clone() });
                let engine_task = tokio::spawn(async move {
                    engine.run().await;
                    let _ = tx_engine.send(ui::UiMsg::EngineStopped { source });
                });
                // 历史轮询（远程服务器历史列表）
                let http = client::transport::HttpTransport::new(&cfg, &password);
                let tx_history = tx.clone();
                let history_task = tokio::spawn(async move {
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
                    loop {
                        interval.tick().await;
                        if let Ok(resp) = http.fetch_history(100).await {
                            let _ = tx_history.send(ui::UiMsg::History { items: resp.items });
                        }
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
        ui::run_ui(ui_cfg, settings::AppSettings::default(), ui_tx, ui_cmd_tx, rx, false, String::new())
            .map_err(|e| e.into())
    }

    #[cfg(not(feature = "tray"))]
    {
        // 精简构建：纯后台 daemon（无托盘）
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let clipboard = Arc::new(SystemClipboard::new()?);
            let mut engine = SyncEngine::new(cfg, password, clipboard);
            engine.run().await;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })
    }
}

/// Form 2：桌面形态。同进程启动 server + client 引擎。
#[cfg(feature = "tray")]
fn run_desktop() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    desktop::run()
}
