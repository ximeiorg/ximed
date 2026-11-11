// Windows：带 tray 时是 GUI 子系统（双击不弹控制台窗口）；
// --no-default-features 的 daemon 形态保留 console，日志要能被重定向/查看。
#![cfg_attr(all(windows, feature = "tray"), windows_subsystem = "windows")]

use clap::Parser;

/// ximed 仅客户端（Form 3）：连远程服务器同步剪贴板（桌面 UI + 托盘；无 UI 时退化为 daemon）。
#[derive(Parser)]
#[command(name = "ximed-client", version, about)]
struct Cli;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Cli::parse();
    app::run_client()
}
