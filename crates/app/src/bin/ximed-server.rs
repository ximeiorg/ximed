use clap::Parser;

/// ximed 仅服务器（Form 1，无 UI）。低性能设备/云服务器部署。
#[derive(Parser)]
#[command(name = "ximed-server", version, about)]
struct Cli;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Cli::parse();
    app::run_server()
}
