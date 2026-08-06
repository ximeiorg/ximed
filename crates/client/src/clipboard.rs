//! 平台剪贴板适配：抽象 get/set，arboard 实现（macOS/Linux/Windows 纯 Rust，无 openssl）。
//!
//! 自写抑制：`set` 会记录写入的 hash，轮询时与 [`ClipboardAdapter::get`] 结果比对，
//! 相同则判定为自身写入的回声，由引擎跳过（避免同步环）。

use std::sync::Mutex;

use domain::hash::text_hash;

/// 剪贴板访问抽象。
pub trait ClipboardAdapter: Send + Sync {
    /// 读取当前剪贴板文本，空剪贴板或无文本返回 None。
    fn get(&self) -> Option<String>;
    /// 写入剪贴板文本。
    fn set(&self, text: &str);
    /// 是否发生了非自身写入的剪贴板变更（供轮询引擎判断）。
    fn changed_since_last_read(&self) -> bool;
}

/// arboard 实现的系统剪贴板（跨平台）。
///
/// `Clipboard` 非 `Sync`，用 `Mutex` 包裹以便在 tokio 多线程 runtime 中共享。
pub struct SystemClipboard {
    inner: Mutex<arboard::Clipboard>,
    /// 上一次 `get` 读到的文本 hash（用于变更检测）。
    last_seen: Mutex<Option<String>>,
}

impl SystemClipboard {
    pub fn new() -> Result<Self, arboard::Error> {
        Ok(Self {
            inner: Mutex::new(arboard::Clipboard::new()?),
            last_seen: Mutex::new(None),
        })
    }
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self::new().expect("failed to init system clipboard")
    }
}

impl ClipboardAdapter for SystemClipboard {
    fn get(&self) -> Option<String> {
        let text = self.inner.lock().ok()?.get_text().ok();
        let hash = text.as_deref().map(text_hash);
        *self.last_seen.lock().ok()? = hash;
        text
    }

    fn set(&self, text: &str) {
        if let Ok(mut c) = self.inner.lock() {
            let _ = c.set_text(text.to_string());
        }
    }

    fn changed_since_last_read(&self) -> bool {
        let current = self
            .inner
            .lock()
            .ok()
            .and_then(|mut c| c.get_text().ok())
            .map(|t| text_hash(&t));
        let last = self.last_seen.lock().ok().and_then(|g| g.clone());
        match (current, last) {
            (Some(c), Some(l)) => c != l,
            (Some(_), None) => true,
            _ => false,
        }
    }
}

/// 测试用内存剪贴板：记录当前文本，手动模拟外部写入与自写。
#[cfg(test)]
pub struct MockClipboard {
    text: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
impl Default for MockClipboard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl MockClipboard {
    pub fn new() -> Self {
        Self {
            text: std::sync::Mutex::new(None),
        }
    }

    /// 模拟用户在系统层面复制（外部写入，非自写）。
    pub fn simulate_external_copy(&self, text: &str) {
        *self.text.lock().unwrap() = Some(text.to_string());
    }
}

#[cfg(test)]
impl ClipboardAdapter for MockClipboard {
    fn get(&self) -> Option<String> {
        self.text.lock().unwrap().clone()
    }

    fn set(&self, text: &str) {
        *self.text.lock().unwrap() = Some(text.to_string());
    }

    fn changed_since_last_read(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_clipboard_roundtrip() {
        let c = MockClipboard::new();
        assert_eq!(c.get(), None);
        c.simulate_external_copy("hello");
        assert_eq!(c.get().as_deref(), Some("hello"));
        c.set("world");
        assert_eq!(c.get().as_deref(), Some("world"));
    }
}
