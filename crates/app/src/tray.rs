//! 系统托盘 / macOS 菜单栏（Form 2/3 桌面形态，Windows / Linux / macOS）。
//!
//! 产品语义：窗口只是入口，进程常驻；关窗 = 隐藏到托盘，只有托盘菜单的
//! 「退出 ximed」才结束进程。托盘菜单固定三项：
//!
//! - 显示主窗口（从隐藏/最小化恢复）
//! - 隐藏到托盘（常驻后台）
//! - 退出 ximed（结束进程，停止同步与内置 server）
//!
//! 平台差异：
//!
//! - **Windows / macOS**：tray-icon 自己处理平台消息循环；菜单事件走 crossbeam
//!   channel，本模块用独立线程轮询并转发给 iced。
//! - **Linux**：tray-icon 走 GTK + libayatana-appindicator，StatusNotifierItem
//!   注册与 DBusMenu 激活都依赖 **GLib 主上下文**被驱动。iced 的 winit 事件循环
//!   不是 GTK 主循环，只 `gtk::init()` 而不迭代的话，托盘项不会出现在面板上、
//!   菜单点击也没有反应。因此这里在 iced 主线程初始化 GTK，并由 UI 的 200ms
//!   定时器调用 [`pump`] 驱动 GTK 事件；所有 GTK 调用都留在主线程。
//!
//! 其他注意：
//! - 托盘句柄存进 iced 主线程的 thread-local，活到进程结束；退出前调用
//!   [`shutdown`] 主动 drop，让 Windows 立即删除图标、Linux 删除临时 PNG。
//! - 菜单项 id 是 `Rc<MenuId>`（非 Send），轮询线程只捕获 id 字符串。
//! - 托盘图标统一用 `assets/icon.png`（按平台缩放），不再程序化画圆点。

use std::cell::RefCell;
use std::time::Duration;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::ui::UiMsg;

thread_local! {
    /// 托盘句柄：只能在创建它的 iced 主线程上访问。
    /// 退出时显式 drop，让 Windows 立即删除托盘图标、Linux 删除临时 PNG。
    static TRAY: RefCell<Option<tray_icon::TrayIcon>> = const { RefCell::new(None) };
}

#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Linux：GTK 是否已初始化成功；未初始化时 [`pump`] 必须空转。
#[cfg(target_os = "linux")]
static GTK_READY: AtomicBool = AtomicBool::new(false);

/// Linux：初始化 GTK（必须在 iced 主线程、且只调用一次）。
#[cfg(target_os = "linux")]
fn init_gtk() -> Result<(), Box<dyn std::error::Error>> {
    gtk::init().map_err(|e| Box::<dyn std::error::Error>::from(format!("gtk init: {e}")))?;
    GTK_READY.store(true, Ordering::Relaxed);
    Ok(())
}

/// Linux：驱动一次 GTK/GLib 事件循环。
///
/// libayatana-appindicator 的 D-Bus 注册与菜单激活都挂在 GLib 默认上下文上；
/// `gtk_main_iteration_do(false)` 非阻塞地处理一轮，由 UI 定时器周期调用。
/// 非 Linux 平台没有 GTK，函数为空实现，方便调用方不加 cfg。
#[cfg(target_os = "linux")]
pub fn pump() {
    if GTK_READY.load(Ordering::Relaxed) {
        let _ = gtk::main_iteration_do(false);
    }
}

/// 非 Linux 平台：托盘由 tray-icon 自己的平台循环驱动，无需泵。
#[cfg(not(target_os = "linux"))]
pub fn pump() {}

/// 构建托盘图标与菜单，并在独立线程轮询菜单事件。
///
/// 菜单动作经 UI 事件通道发送给 iced：
/// - ShowWindow → 窗口设为 Windowed 并聚焦
/// - HideWindow → 窗口设为 Hidden，进程继续常驻
/// - Quit → iced 退出，进程结束
///
/// 返回 `Err` 表示托盘不可用（如无显示环境、GTK 初始化失败）；调用方据此
/// 决定关窗时是否还能安全地隐藏到托盘。
pub fn setup(tx: crate::ui::UiSender) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(target_os = "linux")]
    init_gtk()?;

    let menu = Menu::new();
    let show_item = MenuItem::new("显示主窗口", true, None);
    let hide_item = MenuItem::new("隐藏到托盘", true, None);
    let quit_item = MenuItem::new("退出 ximed", true, None);
    menu.append_items(&[
        &show_item,
        &hide_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])
    .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;

    let icon = TrayIconBuilder::new()
        .with_icon(tray_icon())
        .with_menu(Box::new(menu))
        .with_tooltip("ximed · 剪贴板同步")
        .build()
        .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;
    // 句柄存进主线程 thread-local：既保证托盘活到进程结束，又能在退出时
    // 主动 drop 让系统立刻移除图标（Windows 不 drop 会留下幽灵图标）。
    TRAY.with(|slot| *slot.borrow_mut() = Some(icon));

    // 事件轮询线程：只捕获可 Send 的 id 字符串，避免把 !Send 的 MenuItem 移入线程。
    let show_id = show_item.id().0.to_string();
    let hide_id = hide_item.id().0.to_string();
    let quit_id = quit_item.id().0.to_string();
    std::thread::spawn(move || {
        let menu_rx = MenuEvent::receiver();
        // macOS 的菜单栏图标点击默认展开菜单，不要在点击时顺手弹主窗口；
        // Windows / Linux 保留「左键点击显示窗口」的桌面习惯。
        #[cfg(not(target_os = "macos"))]
        let tray_rx = tray_icon::TrayIconEvent::receiver();
        loop {
            while let Ok(event) = menu_rx.try_recv() {
                let id = event.id().0.to_string();
                if id == show_id {
                    let _ = tx.send(UiMsg::ShowWindow);
                } else if id == hide_id {
                    let _ = tx.send(UiMsg::HideWindow);
                } else if id == quit_id {
                    let _ = tx.send(UiMsg::Quit);
                }
            }
            #[cfg(not(target_os = "macos"))]
            if let Ok(tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            }) = tray_rx.try_recv()
            {
                let _ = tx.send(UiMsg::ShowWindow);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    Ok(())
}

/// 移除托盘图标（退出前调用）。
///
/// 必须在 iced 主线程调用；非主线程访问 thread-local 会拿到另一份空槽，
/// 这里通过统一从 UI 的 update 调用来保证。
pub fn shutdown() {
    TRAY.with(|slot| {
        slot.borrow_mut().take();
    });
}

/// 托盘图标：编译期嵌入的 `assets/icon.png`，按平台缩放到合适尺寸。
///
/// macOS 菜单栏按 22pt 设计；Windows / Linux 托盘常用 16–32px，给 32px
/// 让高 DPI 屏保留细节（系统会再缩放）。macOS 这里用彩色图标而不是 template，
/// 因为源图带不透明圆角底，转 template 会变成一个实心色块。
fn tray_icon() -> Icon {
    let decoded = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .expect("embedded tray icon must decode")
        .into_rgba8();
    let size = if cfg!(target_os = "macos") { 22 } else { 32 };
    let resized =
        image::imageops::resize(&decoded, size, size, image::imageops::FilterType::Lanczos3);
    Icon::from_rgba(resized.into_raw(), size, size).expect("tray icon rgba valid")
}
