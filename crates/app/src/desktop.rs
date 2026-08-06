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
use client::sync::{SyncEngine, SyncStatus};
use client::transport::HttpTransport;

use crate::settings::{AppSettings, SyncMode};
use crate::ui::{
    run_ui, ui_channel, ui_cmd_channel, UiCmd, UiMsg, UiSender,
};

/// 启动 desktop 形态：阻塞直到进程结束。
pub fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut settings = server::config::Settings::new()?;
    let mut client_cfg = ClientConfig::load()?;
    let config_dir = config_dir(&settings);
    let app_settings = AppSettings::load(&config_dir);

    // desktop 形态：client 连接目标由设置决定。
    // - 本地模式：自动跟随本地 server 的监听地址/端口，无需单独配置。
    // - 远程模式：使用设置面板中配置的远程服务器。
    if app_settings.mode == SyncMode::Remote {
        if !app_settings.remote_url.is_empty() {
            client_cfg.server_url = app_settings.remote_url.clone();
        }
        if !app_settings.remote_username.is_empty() {
            client_cfg.username = app_settings.remote_username.clone();
        }
        if let Some(pw) = app_settings.remote_password.as_ref().filter(|p| !p.is_empty()) {
            client_cfg.password = Some(pw.clone());
        }
    } else {
        client_cfg.server_url = local_server_url(&settings);
    }

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
    // UI → 后台命令通道：server 启停 / 设置 / 打开配置目录
    let (cmd_tx, cmd_rx) = ui_cmd_channel();
    // UI 需要的配置与通道（后台线程的 tx 被 move 进线程，此处单独 clone 给 UI）
    let ui_cfg = client_cfg.clone();
    let ui_tx = tx.clone();
    let ui_cmd_tx = cmd_tx.clone();
    // desktop 模式：server 在本地，可启停；配置目录为当前工作目录
    let local_server = true;

    // 后台线程：tokio runtime 承载 server（受控）+ client + 历史轮询（先于 UI 启动）
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("failed to build runtime: {e}");
                return;
            }
        };
        rt.block_on(desktop_control_loop(cmd_rx, settings, server_pw, client_cfg, tx));
    });

    // 主线程：iced 窗口（阻塞直到关闭）
    run_ui(ui_cfg, app_settings, ui_tx, ui_cmd_tx, rx, local_server, config_dir)
        .map_err(|e| e.into())
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

/// 由本地 server 的监听地址推导 client 连接地址（desktop 同进程自动配置）。
///
/// server 绑定通配地址（0.0.0.0 / ::）时 client 走回环 127.0.0.1；
/// 绑定具体 IP/主机名则原样复用。这样只改 `[server] addr`（含端口），
/// client 无需单独配置即可找到本地 server。
fn local_server_url(settings: &server::config::Settings) -> String {
    let addr = &settings.server().addr;
    let (host, port) = addr.rsplit_once(':').unwrap_or(("127.0.0.1", "8443"));
    let host = match host {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        _ => host,
    };
    format!("http://{host}:{port}")
}

/// 后台服务控制循环：响应 UI 命令，统一管理本地 server、client 同步引擎与历史轮询。
///
/// - server 以 [`server::run_managed`] 在独立任务中运行，通过 watch channel 优雅停止。
/// - client 引擎与历史轮询按当前 [`ClientConfig`] 启动；设置变更（[`UiCmd::ApplySettings`]）
///   时持久化并重建，切换远程模式时同时停止本地 server。
async fn desktop_control_loop(
    cmd_rx: std::sync::mpsc::Receiver<UiCmd>,
    settings: server::config::Settings,
    server_pw: String,
    mut client_cfg: ClientConfig,
    tx: UiSender,
) {
    let addr = settings.server().addr.clone();
    // 当前 server 任务及其 shutdown 发送端
    let mut server_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut shutdown_tx: Option<tokio::sync::watch::Sender<bool>> = None;
    // 当前 client 引擎与历史轮询任务
    let mut engine_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut history_task: Option<tokio::task::JoinHandle<()>> = None;

    spawn_client_services(&mut engine_task, &mut history_task, &client_cfg, &server_pw, &tx);

    // 打开配置文件所在目录（macOS `open`，Linux `xdg-open`）
    let open_dir = || {
        // 相对路径（如默认 data_dir `./data`）会被 Explorer 相对其自身工作目录解析，
        // 导致打开错误目录；统一转绝对路径并确保目录存在。
        let raw = PathBuf::from(config_dir(&settings));
        let dir = std::path::absolute(&raw).unwrap_or(raw);
        std::fs::create_dir_all(&dir).ok();
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "linux") {
            "xdg-open"
        } else {
            "explorer"
        };
        if let Err(e) = std::process::Command::new(cmd).arg(&dir).spawn() {
            tracing::warn!("failed to open config dir {}: {e}", dir.display());
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
                UiCmd::ApplySettings { mode, remote_url, remote_username, remote_password } => {
                    let app_settings = AppSettings {
                        mode,
                        remote_url: remote_url.clone(),
                        remote_username: remote_username.clone(),
                        remote_password: if remote_password.is_empty() {
                            None
                        } else {
                            Some(remote_password.clone())
                        },
                    };
                    if let Err(e) = app_settings.save(&config_dir(&settings)) {
                        tracing::warn!("failed to persist app settings: {e}");
                    }
                    // 更新 client 连接配置
                    match mode {
                        SyncMode::Local => {
                            client_cfg.server_url = local_server_url(&settings);
                            client_cfg.username = settings.auth().username.clone();
                            client_cfg.password = Some(server_pw.clone());
                        }
                        SyncMode::Remote => {
                            if !remote_url.is_empty() {
                                client_cfg.server_url = remote_url;
                            }
                            if !remote_username.is_empty() {
                                client_cfg.username = remote_username;
                            }
                            if !remote_password.is_empty() {
                                client_cfg.password = Some(remote_password);
                            }
                            // 切换远程：停止本地 server（若在运行）
                            if let Some(shutdown) = shutdown_tx.take() {
                                let _ = shutdown.send(true);
                            }
                            if let Some(task) = server_task.take() {
                                let _ = task.await;
                            }
                            let _ = tx.send(UiMsg::ServerStopped { addr: addr.clone() });
                        }
                    }
                    // 用新配置重建 client 引擎 + 历史轮询
                    stop_client_services(&mut engine_task, &mut history_task);
                    spawn_client_services(&mut engine_task, &mut history_task, &client_cfg, &server_pw, &tx);
                    let _ = tx.send(UiMsg::SettingsApplied);
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

/// 启动 client 同步引擎 + 历史轮询任务（按当前 `client_cfg` 连接目标 server）。
///
/// 传输认证密码取 `client_cfg` 的有效密码（本地模式=本地 server 密码，远程模式=远程密码）。
fn spawn_client_services(
    engine_task: &mut Option<tokio::task::JoinHandle<()>>,
    history_task: &mut Option<tokio::task::JoinHandle<()>>,
    cfg: &ClientConfig,
    server_pw: &str,
    tx: &UiSender,
) {
    let effective_pw = cfg.password().unwrap_or_else(|| server_pw.to_string());
    let cb = Arc::new(SystemClipboard::new().expect("failed to init clipboard"));
    let mut engine = SyncEngine::new(cfg.clone(), effective_pw.clone(), cb);
    // 连接状态上报 UI：UI 结合 SyncMode 提示用户（本地→去开启，远程→检查配置）
    let tx_status = tx.clone();
    engine.set_status_handler(Arc::new(move |s: SyncStatus| {
        let _ = tx_status.send(UiMsg::SyncStatus {
            online: s == SyncStatus::Online,
        });
    }));
    let tx_engine = tx.clone();
    let source = cfg.source.clone();
    let _ = tx_engine.send(UiMsg::EngineStarted { source: source.clone() });
    *engine_task = Some(tokio::spawn(async move {
        engine.run().await;
        let _ = tx_engine.send(UiMsg::EngineStopped { source });
    }));

    // 历史轮询（周期拉取剪贴板历史推给 UI 历史面板）
    let http = HttpTransport::new(cfg, &effective_pw);
    let tx_history = tx.clone();
    *history_task = Some(tokio::spawn(async move {
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
    }));
}

/// 停止 client 引擎与历史轮询任务（设置变更时重建）。
fn stop_client_services(
    engine_task: &mut Option<tokio::task::JoinHandle<()>>,
    history_task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(t) = engine_task.take() {
        t.abort();
    }
    if let Some(t) = history_task.take() {
        t.abort();
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
            set_password_file_perms(&f);
            let _ = f.write_all(pw.as_bytes());
            pw
        });
    settings.auth.password = Some(generated.clone());
    generated
}

/// 设置密码文件权限：unix 下为 0600，其它平台为 no-op。
#[cfg(unix)]
fn set_password_file_perms(f: &std::fs::File) {
    use std::os::unix::fs::PermissionsExt;
    let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_password_file_perms(_f: &std::fs::File) {}

/// 生成 32 字节随机密码（base64，43 字符）。
fn random_password() -> String {
    use base64::Engine;
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("getrandom");
    base64::engine::general_purpose::STANDARD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use server::config::Settings;

    fn settings_with_addr(addr: &str) -> Settings {
        let mut s = Settings::default();
        s.server.addr = addr.to_string();
        s
    }

    #[test]
    fn local_server_url_uses_loopback_for_wildcard() {
        assert_eq!(
            local_server_url(&settings_with_addr("0.0.0.0:8443")),
            "http://127.0.0.1:8443"
        );
        assert_eq!(
            local_server_url(&settings_with_addr("127.0.0.1:9999")),
            "http://127.0.0.1:9999"
        );
        assert_eq!(
            local_server_url(&settings_with_addr("[::]:8443")),
            "http://127.0.0.1:8443"
        );
    }

    #[test]
    fn local_server_url_keeps_specific_host() {
        assert_eq!(
            local_server_url(&settings_with_addr("192.168.1.5:8443")),
            "http://192.168.1.5:8443"
        );
        assert_eq!(
            local_server_url(&settings_with_addr("localhost:8443")),
            "http://localhost:8443"
        );
    }
}
