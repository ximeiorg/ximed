//! 桌面形态（Form 2）：iced UI（主线程）+ server/client 引擎（后台 tokio 线程）。
//!
//! 线程模型：
//! - 主线程：iced `application` 事件循环（窗口），阻塞直到关闭
//! - 后台线程：tokio runtime，响应 UI 命令管理 server 生命周期 + 剪贴板同步引擎 + 历史轮询
//! - 桥接：
//!   - 后台 → UI：mpsc channel（[`UiMsg`]），iced `Subscription` 注入
//!   - UI → 后台：mpsc channel（[`UiCmd`]），UI 按钮触发（开启/重启/关闭 server、打开配置目录）
//!
//! server 不随应用自动启动：用户点击「开启」后由后台线程以 [`server::run_managed`]
//! 启动（优雅停止由 UI 的「关闭/重启」命令触发）。
//!
//! 零配置：server 未配置密码时自动生成随机密码并落盘
//! `{data_dir}/auth_password`（权限 0600），client 复用同一密码连本机 server。

use std::path::PathBuf;
use std::sync::Arc;

use client::clipboard::SystemClipboard;
use client::config::ClientConfig;
use client::sync::SyncEngine;
use client::transport::HttpTransport;

use crate::ui::{
    run_ui, ui_channel, ui_cmd_channel, UiCmd, UiMsg, UiSender,
};

/// 启动 desktop 形态：阻塞直到进程结束。
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut settings = server::config::Settings::new()?;
    let mut client_cfg = ClientConfig::load()?;

    // 认证密码：server 未配置时自动生成随机密码，落盘并注入两端。
    // 已配置（配置文件 password / 环境变量）则直接复用。
    let server_pw = server_password(&mut settings);
    if client_cfg.password().is_none() {
        client_cfg.password = Some(server_pw.clone());
    }

    // 日志级别取 server 配置
    server::init_tracing(&settings.server().log_level);

    // UI 后台事件通道：后台线程发 → iced Subscription 注入；托盘在 UI 事件循环启动后创建
    let (tx, rx) = ui_channel();
    // UI → 后台命令通道：server 启停 / 打开配置目录
    let (cmd_tx, cmd_rx) = ui_cmd_channel();
    // UI 需要的配置与通道（后台线程的 tx 被 move 进线程，此处单独 clone 给 UI）
    let ui_cfg = client_cfg.clone();
    let ui_tx = tx.clone();
    let ui_cmd_tx = cmd_tx.clone();
    // desktop 模式：server 在本地，可启停；配置目录为当前工作目录
    let local_server = true;
    let config_dir = config_dir(&settings);

    // 后台线程：tokio runtime 承载 server（受控）+ client + 历史轮询（先于 UI 启动）
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("failed to build runtime: {e}");
                return;
            }
        };
        rt.block_on(async {
            // 分支 1：剪贴板同步引擎（状态变更经 channel 推给 UI）
            let cb = Arc::new(SystemClipboard::new().expect("failed to init clipboard"));
            let mut engine = SyncEngine::new(client_cfg.clone(), server_pw.clone(), cb);
            let tx_engine = tx.clone();
            let source = client_cfg.source.clone();
            let _ = tx_engine.send(UiMsg::EngineStarted { source: source.clone() });
            let engine_task = tokio::spawn(async move {
                engine.run().await;
                let _ = tx_engine.send(UiMsg::EngineStopped { source });
            });
            // 分支 2：历史轮询（周期拉取剪贴板历史推给 UI 历史面板）
            let http = HttpTransport::new(&client_cfg, &server_pw);
            let tx_history = tx.clone();
            let history_task = tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
                loop {
                    interval.tick().await;
                    match http.fetch_history(100).await {
                        Ok(resp) => {
                            let _ = tx_history.send(UiMsg::History { items: resp.items });
                        }
                        Err(_) => {
                            // 服务器未就绪/历史未启用：静默跳过
                        }
                    }
                }
            });
            // 分支 3：server 生命周期管理（响应 UI 命令）
            let server_loop = server_control_loop(cmd_rx, settings.clone(), tx.clone());

            tokio::select! {
                _ = engine_task => {}
                _ = history_task => {}
                _ = server_loop => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        });
    });

    // 主线程：iced 窗口（阻塞直到关闭）
    run_ui(ui_cfg, ui_tx, ui_cmd_tx, rx, local_server, config_dir).map_err(|e| e.into())
}

/// 配置文件所在目录：优先当前工作目录（ximed.toml 所在），不存在则回退 data_dir。
fn config_dir(settings: &server::config::Settings) -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd.join("ximed.toml").exists() {
        cwd.to_string_lossy().into_owned()
    } else {
        settings.server().data_dir.clone()
    }
}

/// 后台 server 生命周期控制循环：等待 UI 命令并启停 server。
///
/// server 以 [`server::run_managed`] 在独立任务中运行，通过 watch channel 优雅停止。
async fn server_control_loop(
    cmd_rx: std::sync::mpsc::Receiver<UiCmd>,
    settings: server::config::Settings,
    tx: UiSender,
) {
    let addr = settings.server().addr.clone();
    // 当前 server 任务及其 shutdown 发送端
    let mut server_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut shutdown_tx: Option<tokio::sync::watch::Sender<bool>> = None;

    // 打开配置文件所在目录（macOS `open`，Linux `xdg-open`）
    let open_dir = || {
        let dir = config_dir(&settings);
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "linux") {
            "xdg-open"
        } else {
            "explorer"
        };
        if let Err(e) = std::process::Command::new(cmd).arg(&dir).spawn() {
            tracing::warn!("failed to open config dir {dir}: {e}");
        }
    };

    let mut interval = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        interval.tick().await;
        // 处理所有待处理命令
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                UiCmd::ServerStart => {
                    if server_task.is_some() {
                        continue;
                    }
                    let (shutdown_tx_, shutdown_rx) = tokio::sync::watch::channel(false);
                    let settings = settings.clone();
                    let task = tokio::spawn(async move {
                        if let Err(e) = server::run_managed(settings, shutdown_rx).await {
                            tracing::error!("server exited: {e}");
                        }
                    });
                    shutdown_tx = Some(shutdown_tx_);
                    server_task = Some(task);
                    let _ = tx.send(UiMsg::ServerListening { addr: addr.clone() });
                }
                UiCmd::ServerStop => {
                    if let Some(shutdown) = shutdown_tx.take() {
                        let _ = shutdown.send(true);
                    }
                    if let Some(task) = server_task.take() {
                        let _ = task.await;
                    }
                    let _ = tx.send(UiMsg::ServerStopped { addr: addr.clone() });
                }
                UiCmd::ServerRestart => {
                    if let Some(shutdown) = shutdown_tx.take() {
                        let _ = shutdown.send(true);
                    }
                    if let Some(task) = server_task.take() {
                        let _ = task.await;
                    }
                    let (shutdown_tx_, shutdown_rx) = tokio::sync::watch::channel(false);
                    let settings = settings.clone();
                    let task = tokio::spawn(async move {
                        if let Err(e) = server::run_managed(settings, shutdown_rx).await {
                            tracing::error!("server exited: {e}");
                        }
                    });
                    shutdown_tx = Some(shutdown_tx_);
                    server_task = Some(task);
                    let _ = tx.send(UiMsg::ServerListening { addr: addr.clone() });
                }
                UiCmd::OpenConfigDir => {
                    open_dir();
                }
            }
        }
        // server 任务异常退出时同步 UI 状态（如端口被占用）
        if let Some(task) = server_task.as_mut() {
            if task.is_finished() {
                let _ = tx.send(UiMsg::ServerStopped { addr: addr.clone() });
                server_task = None;
                shutdown_tx = None;
            }
        }
    }
}

/// 解析 server 认证密码：已配置则直接返回；否则生成随机密码并落盘
/// `{data_dir}/auth_password`（0600），注入 settings.auth.password。
fn server_password(settings: &mut server::config::Settings) -> String {
    if let Some(pw) = settings.auth().password() {
        return pw;
    }
    let data_dir = PathBuf::from(settings.server().data_dir.clone());
    let pw_file = data_dir.join("auth_password");
    // 已有落盘密码则复用（重启保持稳定）
    let generated = std::fs::read_to_string(&pw_file)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let pw = random_password();
            std::fs::create_dir_all(&data_dir).ok();
            use std::io::Write;
            let mut f = std::fs::File::create(&pw_file).expect("create auth_password file");
            let _ = f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600));
            let _ = f.write_all(pw.as_bytes());
            pw
        });
    settings.auth.password = Some(generated.clone());
    generated
}

/// 生成 32 字节随机密码（base64，43 字符）。
fn random_password() -> String {
    use base64::Engine;
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("getrandom");
    base64::engine::general_purpose::STANDARD.encode(buf)
}
