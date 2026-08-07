//! macOS 平台专用：运行时设置 Dock 图标。
//!
//! `cargo run` 裸跑二进制时没有 `.app` 包，macOS 不会从 `.icns` 加载 Dock 图标，
//! 需要运行时调用 `NSApplication setApplicationIconImage:`。
//! 打包成 `.app`（cargo-packager 生成）后 `.icns` 由系统加载，此调用无害。

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage};
use objc2_foundation::{NSData, NSUInteger};

/// 用嵌入的 PNG 设置 Dock 图标。失败仅打日志，不阻断启动。
pub fn set_dock_icon() {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let marker =
            MainThreadMarker::new().ok_or("NSApplication must be set on the main thread")?;
        let bytes = include_bytes!("../assets/icon.png");
        let image = load_nsimage(bytes)?;
        let app = NSApplication::sharedApplication(marker);
        unsafe {
            app.setApplicationIconImage(Some(&image));
        }
        Ok(())
    })();

    if let Err(e) = result {
        eprintln!("[ximed] failed to set dock icon: {e}");
        tracing::warn!("failed to set dock icon: {e}");
    }
}

/// 从 PNG 字节构造 NSImage。
fn load_nsimage(bytes: &[u8]) -> Result<Retained<NSImage>, Box<dyn std::error::Error>> {
    let data = unsafe {
        NSData::dataWithBytes_length(bytes.as_ptr() as *const c_void, bytes.len() as NSUInteger)
    };
    let image = NSImage::initWithData(NSImage::alloc(), &data).ok_or("NSImage init failed")?;
    Ok(image)
}
