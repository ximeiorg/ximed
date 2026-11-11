// Windows：GUI 子系统。少了这行，双击 exe 会先弹一个控制台窗口
// （黑窗）并一直挂到进程退出——那是 console 子系统的默认行为，
// 与 server / 同步引擎无关。
#![cfg_attr(windows, windows_subsystem = "windows")]

use clap::Parser;

/// ximed 桌面形态（Form 2）：同进程 server + client + 托盘（桌面 UI）。
#[derive(Parser)]
#[command(name = "ximed-desktop", version, about)]
struct Cli;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Cli::parse();
    app::run_desktop()
}
