//! ximed 服务器库：提供可复用的服务器启动逻辑。
//!
//! - `main.rs`（二进制 `server`）：薄入口，加载配置后调用 [`run`]。
//! - `app` crate（`ximed` 二进制）：desktop 模式在后台 tokio 任务中调用 [`run`]，
//!   与 client 引擎同进程共存。
//!
//! 注意：调用 [`run`] 前需要先初始化 tracing（见 [`init_tracing`]）。

pub mod auth;
pub mod config;
pub mod handlers;
pub mod state;
pub mod tls;

use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use config::Settings;

/// 初始化 tracing 日志（幂等，重复调用自动忽略）。
///
/// 过滤器按配置级别放行本项目各 crate（server/service/client/app），
/// 便于排查客户端推送、历史轮询等链路问题。
pub fn init_tracing(level: &str) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(format!(
            "server={level},service={level},client={level},app={level}"
        )))
        .try_init();
}

/// 校验认证密码是否已配置（配置文件 `auth.password` 或环境变量）。
pub fn check_auth_password(settings: &Settings) -> Result<(), String> {
    if settings.auth().password().is_some() {
        Ok(())
    } else {
        Err(format!(
            "auth password not set; set auth.password in config file, or export {}",
            settings.auth().password_env
        ))
    }
}

/// 启动服务器并阻塞直到退出。
///
/// 装配 storage / history / AppState / 路由，按 tls.enabled 选择 HTTPS 或 HTTP。
pub async fn run(settings: Settings) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_managed(settings, tokio::sync::watch::channel(false).1).await
}

/// 受管启动：额外接受一个 shutdown 信号，收到后优雅停止（HTTP 用
/// `with_graceful_shutdown`，TLS 用 `Handle::graceful_shutdown`）。
///
/// desktop 形态用它实现 UI 上的「开启 / 重启 / 关闭」服务控制。
pub async fn run_managed(
    settings: Settings,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 数据目录
    std::fs::create_dir_all(&settings.server().data_dir)?;

    let storage = store::build_storage(
        store::BackendKind::from_name(&settings.storage().backend),
        &store::StorageOptions {
            data_dir: settings.server().data_dir.clone(),
            webdav_url: settings.storage().url.clone(),
            webdav_username: settings.storage().username.clone(),
            webdav_password: settings.storage().webdav_password(),
        },
    );

    // SQLite 历史记录（history.db 与数据目录同根，按配置保留上限修剪）
    let history_repo = match store::HistoryRepo::open_with_limit(
        std::path::Path::new(&settings.server().data_dir).join("history.db"),
        settings.history().max_items,
    ) {
        Ok(repo) => {
            tracing::info!(
                "history db enabled (max_items={})",
                settings.history().max_items
            );
            Some(Arc::new(repo))
        }
        Err(e) => {
            tracing::warn!("history db disabled: {e}");
            None
        }
    };

    let clipboard_ctx = service::ClipboardContext::with_history(storage, history_repo);
    let app_state = Arc::new(
        state::AppState::new(
            clipboard_ctx,
            settings.auth().clone(),
            settings.clipboard().clone(),
            settings.sse().clone(),
        )
        .with_history_cfg(settings.history().clone()),
    );

    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(handlers::http::healthz))
        .route("/api/version", axum::routing::get(handlers::http::version))
        .route(
            "/api/clipboard",
            axum::routing::get(handlers::http::get_clipboard).put(handlers::http::put_clipboard),
        )
        .route(
            "/api/clipboard/history",
            axum::routing::get(handlers::http::get_history),
        )
        .route(
            "/api/clipboard/file",
            axum::routing::get(handlers::http::get_file),
        );
    // WS 通道（可配置启停，端点为固定协议路径 /sync）
    let app = if settings.ws().enabled {
        app.route("/sync", axum::routing::get(handlers::ws::ws_handler))
    } else {
        app
    };
    // SSE 通道（可配置启停，端点为固定协议路径 /events）
    let app = if settings.sse().enabled {
        app.route("/events", axum::routing::get(handlers::sse::sse_handler))
    } else {
        app
    };
    let app = app
        .layer(handlers::http::body_limit(
            settings.clipboard().max_frame_size,
        ))
        .with_state(app_state);

    let addr: std::net::SocketAddr = settings.server().addr.parse()?;

    // shutdown 信号：watch receiver 变为 true 时触发
    async fn wait_shutdown(rx: &mut tokio::sync::watch::Receiver<bool>) {
        loop {
            if *rx.borrow() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    // TLS：tls.enabled 时用 axum-server + rustls 提供 HTTPS（参考 axum example-tls-rustls）
    if settings.tls().enabled {
        let tls_config = tls::build_rustls_config(settings.tls()).await?;
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        let shutdown_task = async {
            wait_shutdown(&mut shutdown).await;
            shutdown_handle.graceful_shutdown(None);
        };
        tracing::info!(
            "ximed {} (https) listening on {}",
            env!("CARGO_PKG_VERSION"),
            addr
        );
        tokio::select! {
            result = axum_server::bind_rustls(addr, tls_config)
                .handle(handle)
                .serve(app.into_make_service()) => result?,
            _ = shutdown_task => {}
        }
    } else {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        tracing::info!("ximed {} listening on {}", env!("CARGO_PKG_VERSION"), addr);
        let shutdown_wait = async move {
            wait_shutdown(&mut shutdown).await;
        };
        tokio::select! {
            result = axum::serve(listener, app).with_graceful_shutdown(shutdown_wait) => result?,
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Settings;
    use super::run_managed;
    use crate::config::{self, AuthConfig, ServerConfig};
    use std::net::TcpListener;

    /// 构造指向临时目录 + 随机端口的 Settings（HTTP 路径）。
    fn test_settings() -> (Settings, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        // 预绑定再释放拿到空闲端口
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let s = Settings {
            server: ServerConfig {
                addr: format!("127.0.0.1:{port}"),
                data_dir: dir.path().to_string_lossy().into_owned(),
                workers: None,
                log_level: "error".to_string(),
            },
            auth: AuthConfig {
                username: "alice".to_string(),
                password: Some("pw".to_string()),
                password_env: "XIMED_AUTH_PASSWORD".to_string(),
                max_connections: 32,
            },
            clipboard: config::ClipboardConfig {
                max_frame_size: 1024,
                idle_timeout_secs: 60,
                heartbeat_interval_secs: 30,
                default_profile: None,
            },
            http: config::HttpConfig::default(),
            ws: config::WsConfig::default(),
            sse: config::SseConfig::default(),
            storage: config::StorageConfig {
                backend: "local".to_string(),
                url: None,
                username: None,
                password_env: None,
                endpoint: None,
                bucket: None,
                region: None,
                access_key_env: None,
                secret_key_env: None,
                auth_token_env: None,
            },
            tls: config::TlsConfig::default(),
            history: config::HistoryConfig::default(),
        };
        (s, dir)
    }

    /// `run_managed` 收到 shutdown 信号后优雅退出，而非一直挂起。
    #[tokio::test]
    async fn run_managed_stops_on_shutdown_signal() {
        let (settings, _dir) = test_settings();
        let addr = settings.server.addr.clone();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(run_managed(settings, rx));

        // 等服务启动（重试 connect 直到成功）
        let mut ready = false;
        for _ in 0..100 {
            if std::net::TcpStream::connect(&addr).is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(ready, "server should start listening on {addr}");

        let _ = tx.send(true);

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
        assert!(
            result.is_ok(),
            "server must exit after shutdown signal, got timeout"
        );
        assert!(
            result.unwrap().is_ok(),
            "server run_managed should return Ok"
        );
    }
}
