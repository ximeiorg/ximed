//! 同步设置（对标 SyncClipboard SyncConfig）。
//!
//! 由 `app` crate 持久化到 `ximed-client.toml`，传给 [`crate::sync::SyncEngine`] 遵循。
//! 定义在 client crate 而非 app crate：引擎（client）消费它，依赖方向 app → client。

use serde::{Deserialize, Serialize};

/// 同步设置。
///
/// 当前引擎只同步文本：`upload_image` / `upload_single_file` 为类型白名单的
/// 配置框架，实际图片/文件同步能力作为后续功能点实现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSettings {
    /// 同步主开关（默认开：本地剪贴板变化即推送到服务器，服务器变化即写回本地）。
    #[serde(default = "default_true")]
    pub switch_on: bool,
    /// 推送：本地剪贴板变化 → 上传到服务器（默认开）。
    #[serde(default = "default_true")]
    pub push_switch_on: bool,
    /// 拉取：服务器变化 → 下载写回本地（默认开）。
    #[serde(default = "default_true")]
    pub pull_switch_on: bool,
    /// 允许上传文本。
    #[serde(default = "default_true")]
    pub upload_text: bool,
    /// 允许上传图片（框架预留，引擎当前仅文本）。
    #[serde(default = "default_true")]
    pub upload_image: bool,
    /// 允许上传单文件（框架预留，引擎当前仅文本）。
    #[serde(default = "default_true")]
    pub upload_single_file: bool,
    /// 上传附件大小上限（字节），默认 10MB。
    #[serde(default = "default_max_file_byte")]
    pub max_file_byte: u64,
    /// 收到远端内容时通知用户（默认开）。
    #[serde(default = "default_true")]
    pub notify_on_downloaded: bool,
}

impl Default for SyncSettings {
    fn default() -> Self {
        Self {
            switch_on: true,
            push_switch_on: true,
            pull_switch_on: true,
            upload_text: true,
            upload_image: true,
            upload_single_file: true,
            max_file_byte: default_max_file_byte(),
            notify_on_downloaded: true,
        }
    }
}

impl SyncSettings {
    /// 该类型是否允许上传（类型白名单：text/image/file，group 多文件不开放）。
    pub fn type_allowed(&self, ty: domain::profile::ProfileType) -> bool {
        use domain::profile::ProfileType;
        match ty {
            ProfileType::Text => self.upload_text,
            ProfileType::Image => self.upload_image,
            ProfileType::File => self.upload_single_file,
            ProfileType::Group | ProfileType::None => false,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_max_file_byte() -> u64 {
    10 * 1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::profile::ProfileType;

    #[test]
    fn sync_settings_defaults() {
        let s = SyncSettings::default();
        assert!(s.switch_on, "master switch must default on");
        assert!(s.push_switch_on);
        assert!(s.pull_switch_on);
        assert!(s.upload_text);
        assert!(s.upload_image);
        assert!(s.upload_single_file);
        assert_eq!(s.max_file_byte, 10 * 1024 * 1024);
        assert!(s.notify_on_downloaded);
    }

    #[test]
    fn type_allowed_matches_switches() {
        let s = SyncSettings::default();
        assert!(s.type_allowed(ProfileType::Text));
        assert!(s.type_allowed(ProfileType::Image));
        assert!(s.type_allowed(ProfileType::File));
        // 多文件（group）与 none 一律不允许
        assert!(!s.type_allowed(ProfileType::Group));
        assert!(!s.type_allowed(ProfileType::None));

        let off = SyncSettings {
            upload_text: false,
            upload_image: false,
            upload_single_file: false,
            ..SyncSettings::default()
        };
        assert!(!off.type_allowed(ProfileType::Text));
        assert!(!off.type_allowed(ProfileType::Image));
        assert!(!off.type_allowed(ProfileType::File));
    }
}
