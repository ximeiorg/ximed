//! `server` 二进制薄入口：加载配置、校验密码、构建 runtime、调用库的 [`run`]。
//!
//! 服务器实现全部在 `lib.rs`（供 desktop 模式复用）。

use server::config::Settings;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let settings = Settings::new()?;

    // 敏感项校验：认证密码必须通过环境变量提供
    if let Err(msg) = server::check_auth_password(&settings) {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }

    server::init_tracing(&settings.server().log_level);

    // 按配置构建 tokio runtime：workers 可配，低端设备可设为 1
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    if let Some(workers) = settings.server().workers {
        builder.worker_threads(workers);
    }
    let rt = builder
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    rt.block_on(server::run(settings))
}
