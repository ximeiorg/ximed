//! 客户端本地剪贴板历史（插件适配器模式使用）。
//!
//! server 适配器模式的历史由 server 的 SQLite `HistoryRepo` 记录、client 经
//! HTTP API 拉取；插件适配器（Lua clipboard_sync 插件）没有 server，本模块复用
//! [`xime_sync_store::HistoryRepo`] 在 client 本地记录剪贴板历史，供历史面板展示。
//!
//! 记录时机：同步引擎每次把本地内容推送到远端、或把远端内容写入本地时，
//! 由上层（app）调用 [`LocalHistory::record`] 记录一条。

use std::sync::Arc;

use xime_sync_domain::profile::{Profile, ProfileType};
use xime_sync_store::HistoryRepo;

/// 客户端本地历史仓库（共享引用，跨线程使用）。
#[derive(Clone)]
pub struct LocalHistory {
    repo: Arc<HistoryRepo>,
}

impl LocalHistory {
    /// 打开（或创建）客户端本地历史库；`max_items` 为保留条数上限。
    ///
    /// 打开失败返回 `None`（历史记录属尽力而为，不阻断同步）。
    pub fn open(db_path: impl Into<std::path::PathBuf>, max_items: u64) -> Option<Self> {
        let repo = HistoryRepo::open_with_limit(db_path, max_items).ok()?;
        Some(Self {
            repo: Arc::new(repo),
        })
    }

    /// 记录一条剪贴板历史（当前仅文本，图片/文件为后续功能点）。
    pub fn record(&self, profile: &Profile) {
        let kind = match profile.r#type {
            ProfileType::Text => "text",
            ProfileType::Image => "image",
            ProfileType::File => "file",
            ProfileType::Group | ProfileType::None => "other",
        };
        // seq：客户端本地无广播序号，用时间戳微秒近似（同秒多条靠后者覆盖前者，
        // 对历史面板展示影响极小；后续可改自增列）。
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let entry = xime_sync_store::history::HistoryEntry {
            seq,
            hash: profile.hash.clone(),
            kind: kind.to_string(),
            text: profile.text.clone(),
            source: profile.source.clone(),
            created_at: seq as i64 / 1_000_000,
        };
        if let Err(e) = self.repo.insert(&entry) {
            tracing::warn!("local history insert failed: {e}");
        }
    }

    /// 查询最近 `limit` 条历史（时间倒序），供历史面板展示。
    pub fn list(&self, limit: usize) -> Vec<crate::history::HistoryItem> {
        match self.repo.list(limit, None) {
            Ok(entries) => entries
                .into_iter()
                .map(|e| crate::history::HistoryItem {
                    seq: e.seq,
                    hash: e.hash,
                    kind: e.kind,
                    text: e.text,
                    source: e.source,
                    created_at: e.created_at,
                })
                .collect(),
            Err(e) => {
                tracing::warn!("local history query failed: {e}");
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(tmp: &tempfile::TempDir, max: u64) -> LocalHistory {
        LocalHistory::open(tmp.path().join("h.db"), max).unwrap()
    }

    #[test]
    fn record_and_list_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let h = hist(&dir, 100);
        let p = Profile::from_text("hello", Some("dev-a".to_string()));
        h.record(&p);
        let items = h.list(100);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "hello");
        assert_eq!(items[0].kind, "text");
        assert_eq!(items[0].source.as_deref(), Some("dev-a"));
    }

    #[test]
    fn prunes_beyond_max() {
        let dir = tempfile::tempdir().unwrap();
        let h = hist(&dir, 3);
        for i in 0..5 {
            h.record(&Profile::from_text(
                format!("item{i}"),
                Some("dev".to_string()),
            ));
        }
        assert!(h.list(100).len() <= 3, "must prune to max_items");
    }
}
