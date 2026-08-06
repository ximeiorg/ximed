//! 托盘/菜单栏（Form 2/3 桌面形态）。
//!
//! tray-icon 的事件通道（`TrayIconEvent::receiver` / `MenuEvent::receiver`）是
//! crossbeam channel，可在任意线程轮询——无需挂进 GUI 事件循环。
//! 本模块在后台线程轮询托盘事件，菜单动作经 mpsc channel 转发给 iced UI。
//!
//! 注意：
//! - menu item 的 `id` 是 `Rc<MenuId>`（非 Send），不能移入轮询线程；
//!   这里在回调闭包内用 `match` 判定，轮询线程只转发 `MenuEvent`。
//! - macOS 必须提供图标（`with_icon`），否则菜单栏不显示托盘项。

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::ui::UiMsg;

/// 构建托盘图标与菜单，并在独立线程轮询事件。
///
/// 菜单「显示窗口 / 隐藏窗口 / 退出」经 UI 事件通道发送给 iced：
/// - ShowWindow → UI 把窗口设为 Windowed（从托盘恢复）
/// - HideWindow → UI 把窗口设为 Hidden（隐藏到托盘）
/// - Quit → 发送专用消息由 UI 处理退出
/// 当前 iced 窗口常驻，托盘主要提供状态提示与显示/隐藏入口。
///
/// 注意：`TrayIcon` 被 drop 时系统会移除托盘图标，因此这里用 `Box::leak`
/// 使其存活整个进程（托盘生命周期 = 进程生命周期），不能返回给调用方持有。
pub fn setup(tx: crate::ui::UiSender) -> Result<(), Box<dyn std::error::Error>> {
    let menu = Menu::new();
    let status_item = MenuItem::new("状态", true, None);
    let show_item = MenuItem::new("显示窗口", true, None);
    let hide_item = MenuItem::new("隐藏窗口", true, None);
    let quit_item = MenuItem::new("退出", true, None);
    menu.append_items(&[
        &status_item,
        &show_item,
        &hide_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])
    .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;

    let icon = TrayIconBuilder::new()
        .with_icon(make_icon())
        .with_menu(Box::new(menu))
        .with_tooltip("ximed")
        .build()
        .map_err(|e| Box::<dyn std::error::Error>::from(e.to_string()))?;
    Box::leak(Box::new(icon));

    // 事件轮询线程：菜单事件 + 托盘事件（左键点击显示窗口）。
    // 只捕获可 Send 的 id 字符串与回调，避免把 !Send 的 MenuItem 移入线程。
    let status_id = status_item.id().0.to_string();
    let show_id = show_item.id().0.to_string();
    let hide_id = hide_item.id().0.to_string();
    let quit_id = quit_item.id().0.to_string();
    std::thread::spawn(move || {
        let menu_rx = MenuEvent::receiver();
        let tray_rx = tray_icon::TrayIconEvent::receiver();
        loop {
            if let Ok(event) = menu_rx.try_recv() {
                let id = event.id().0.to_string();
                if id == status_id {
                    let _ = tx.send(UiMsg::EngineStarted {
                        source: "tray".to_string(),
                    });
                } else if id == show_id {
                    let _ = tx.send(UiMsg::ShowWindow);
                } else if id == hide_id {
                    let _ = tx.send(UiMsg::HideWindow);
                } else if id == quit_id {
                    // 退出：由 UI 处理（退出按钮同路径）
                    let _ = tx.send(UiMsg::Quit);
                }
            }
            if let Ok(event) = tray_rx.try_recv() {
                if let tray_icon::TrayIconEvent::Click { .. } = event {
                    let _ = tx.send(UiMsg::ShowWindow);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    Ok(())
}

/// 生成托盘图标：32×32 RGBA，主色圆点（与 UI 主题一致的蓝）。
///
/// macOS 上若无图标（`with_icon`）托盘项不显示，这里程序化生成避免依赖资源文件。
fn make_icon() -> Icon {
    let size = 32u32;
    // 主色 #7aa2f7，圆点半径
    let (cr, cg, cb) = (0x7a, 0xa2, 0xf7);
    let radius = 13.0f32;
    let center = size as f32 / 2.0;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let inside = (dx * dx + dy * dy).sqrt() <= radius;
            // 抗锯齿：边缘 1px 渐变
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = if inside {
                255
            } else {
                let edge = (dist - radius).clamp(0.0, 1.0);
                (255.0 * (1.0 - edge)) as u8
            };
            rgba.push(cr);
            rgba.push(cg);
            rgba.push(cb);
            rgba.push(alpha);
        }
    }
    Icon::from_rgba(rgba, size, size).expect("tray icon rgba valid")
}
