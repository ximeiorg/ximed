use clap::Parser;

/// ximed 桌面形态（Form 2）：同进程 server + client + 托盘（桌面 UI）。
#[derive(Parser)]
#[command(name = "ximed-desktop", version, about)]
struct Cli;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Cli::parse();
    app::run_desktop()
}
