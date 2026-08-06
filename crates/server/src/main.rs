mod auth;
mod config;
mod handlers;
mod state;
mod tls;

use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use config::Settings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings = Settings::new()?;

    // 敏感项校验：认证密码必须通过环境变量提供
    if settings.auth().password().is_none() {
        eprintln!(
            "error: auth password not set; export {} (or set auth.password_env)",
            settings.auth().password_env
        );
        std::process::exit(1);
    }

    // 按配置构建 tokio runtime：workers 可配，低端设备可设为 1
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(workers) = settings.server().workers {
        builder.worker_threads(workers);
    }
    let rt = builder
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(async_main(settings))
}

async fn async_main(settings: Settings) -> Result<(), Box<dyn std::error::Error>> {
    let filter = EnvFilter::new(format!("server={}", settings.server().log_level));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // 数据目录
    std::fs::create_dir_all(&settings.server().data_dir)?;

    let storage = store::build_storage(
        store::BackendKind::from_str(&settings.storage().backend),
        &store::StorageOptions {
            data_dir: settings.server().data_dir.clone(),
            webdav_url: settings.storage().url.clone(),
            webdav_username: settings.storage().username.clone(),
            webdav_password: settings.storage().webdav_password(),
        },
    );

    // SQLite 历史记录（history.db 与数据目录同根）
    let history_repo = match store::HistoryRepo::open(
        std::path::Path::new(&settings.server().data_dir).join("history.db"),
    ) {
        Ok(repo) => {
            tracing::info!("history db enabled");
            Some(Arc::new(repo))
        }
        Err(e) => {
            tracing::warn!("history db disabled: {e}");
            None
        }
    };

    let clipboard_ctx = service::ClipboardContext::with_history(storage, history_repo);
    let app_state = Arc::new(state::AppState::new(
        clipboard_ctx,
        settings.auth().clone(),
        settings.clipboard().clone(),
        settings.sse().clone(),
    ));

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
        .layer(handlers::http::body_limit(settings.clipboard().max_frame_size))
        .with_state(app_state);

    let addr: std::net::SocketAddr = settings.server().addr.parse()?;

    // TLS：tls.enabled 时用 axum-server + rustls 提供 HTTPS（参考 axum example-tls-rustls）
    if settings.tls().enabled {
        let tls_config = tls::build_rustls_config(settings.tls()).await?;
        tracing::info!("ximed {} (https) listening on {}", env!("CARGO_PKG_VERSION"), addr);
        axum_server::bind_rustls(addr, tls_config)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        tracing::info!("ximed {} listening on {}", env!("CARGO_PKG_VERSION"), addr);
        axum::serve(listener, app).await?;
    }
    Ok(())
}
