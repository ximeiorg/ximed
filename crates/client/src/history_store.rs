//! 客户端本地剪贴板历史（插件适配器模式使用）。
//!
//! server 适配器模式的历史由 server 的 SQLite `HistoryRepo` 记录、client 经
//! HTTP API 拉取；插件适配器（Lua clipboard_sync 插件）没有 server，本模块复用
//! [`xime_sync_store::HistoryRepo`] 在 client 本地记录剪贴板历史，供历史面板展示。
//!
//! 记录时机：同步引擎每次把本地内容推送到远端、或把远端内容写入本地时，
//! 由上层（app）调用 [`LocalHistory::record`] 记录一条。
//!
//! 去重：**同一个内容（hash）只保留一条**。重复触发 record 的来源不少——引擎重启
//! 后的首次推送、同时开了两个实例、同一段文本反复复制——不去重的话历史面板里
//! 会堆出一串一模一样的条目。再次复制旧内容时把它移到最新（等价于「置顶」），
//! 而不是新增一条。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use xime_sync_domain::profile::{Profile, ProfileType};
use xime_sync_store::HistoryRepo;

/// 客户端本地历史仓库（共享引用，跨线程使用）。
#[derive(Clone)]
pub struct LocalHistory {
    repo: Arc<HistoryRepo>,
    /// 写连接：按 hash 去重 + 修剪。
    ///
    /// `HistoryRepo` 只有追加接口、没有删除能力，而「重复内容移到最新」需要先按
    /// hash 删旧行，所以这里自己开一条只用于写入的连接（读仍然走 `repo`）。
    write: Arc<Mutex<Connection>>,
    /// 保留条数上限（写入后修剪，与列表保持一致）。
    max_items: usize,
}

impl LocalHistory {
    /// 打开（或创建）客户端本地历史库；`max_items` 为保留条数上限。
    ///
    /// 打开失败返回 `None`（历史记录属尽力而为，不阻断同步）。
    pub fn open(db_path: impl Into<PathBuf>, max_items: u64) -> Option<Self> {
        let path = db_path.into();
        let repo = HistoryRepo::open_with_limit(&path, max_items).ok()?;
        let conn = Connection::open(&path).ok()?;
        // 多条连接共用同一个库：等锁而不是立刻报 SQLITE_BUSY
        let _ = conn.busy_timeout(Duration::from_secs(2));
        // 旧版本没有按 hash 去重：升级后清一次历史里已有的重复（每个 hash 留最新一条）
        let _ = conn.execute(
            "DELETE FROM clipboard_history
             WHERE seq NOT IN (SELECT MAX(seq) FROM clipboard_history GROUP BY hash)",
            [],
        );
        Some(Self {
            repo: Arc::new(repo),
            write: Arc::new(Mutex::new(conn)),
            max_items: max_items.max(1) as usize,
        })
    }

    /// 记录一条剪贴板历史（当前仅文本，图片/文件为后续功能点）。
    ///
    /// 同一个 hash 只保留一条：先删旧行再插入，于是「再次复制同一段内容」= 移到最新。
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

        let Ok(conn) = self.write.lock() else { return };
        let outcome = conn.unchecked_transaction().and_then(|tx| {
            tx.execute(
                "DELETE FROM clipboard_history WHERE hash = ?1",
                rusqlite::params![entry.hash],
            )?;
            tx.execute(
                "INSERT INTO clipboard_history (seq, hash, kind, text, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    entry.seq as i64,
                    entry.hash,
                    entry.kind,
                    entry.text,
                    entry.source,
                    entry.created_at,
                ],
            )?;
            // 保留上限修剪：仅保留最新的 max_items 条
            tx.execute(
                "DELETE FROM clipboard_history
                 WHERE seq IN (
                     SELECT seq FROM clipboard_history
                     ORDER BY seq DESC LIMIT -1 OFFSET ?1
                 )",
                rusqlite::params![self.max_items as i64],
            )?;
            tx.commit()
        });
        if let Err(e) = outcome {
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

    /// 同一个内容只保留一条；再次复制同一段文本 = 移到最新。
    #[test]
    fn duplicate_content_is_deduped_and_moved_to_top() {
        let dir = tempfile::tempdir().unwrap();
        let h = hist(&dir, 100);
        h.record(&Profile::from_text("first", None));
        h.record(&Profile::from_text("second", None));
        h.record(&Profile::from_text("first", None));

        let items = h.list(100);
        assert_eq!(items.len(), 2, "同一内容不能出现两条");
        assert_eq!(items[0].text, "first", "重复复制应移到最新");
    }

    /// 旧版本留下的重复：打开时清一次，每个 hash 保留最新一条。
    #[test]
    fn legacy_duplicates_are_cleaned_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        {
            // 用上游的追加接口模拟旧行为：同一 hash 写两条
            let repo = xime_sync_store::HistoryRepo::open_with_limit(&path, 100).unwrap();
            for seq in [1u64, 2] {
                repo.insert(&xime_sync_store::history::HistoryEntry {
                    seq,
                    hash: "same-hash".to_string(),
                    kind: "text".to_string(),
                    text: "dup".to_string(),
                    source: None,
                    created_at: seq as i64,
                })
                .unwrap();
            }
        }

        let h = LocalHistory::open(path, 100).unwrap();
        let items = h.list(100);
        assert_eq!(items.len(), 1, "打开时应清掉历史重复");
        assert_eq!(items[0].seq, 2, "应保留最新一条");
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
