use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

/// 单条剪贴板历史记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// 事件序号（与广播 seq 一致）。
    pub seq: u64,
    /// 剪贴板内容 hash。
    pub hash: String,
    /// 类型（text/file/image/...）。
    pub kind: String,
    /// 文本内容或预览。
    pub text: String,
    /// 来源设备。
    pub source: Option<String>,
    /// 记录时间（Unix 秒）。
    pub created_at: i64,
}

/// SQLite 历史记录仓库（feature = history 启用）。
///
/// 手写 SQL（决策记录 9：历史查询复杂度低，不引入 ORM）。
/// 本地嵌入式 SQLite 文件，无网络依赖。
/// 用 Mutex 包裹 Connection 以满足 Send+Sync（rusqlite Connection 本身非 Sync）。
pub struct HistoryRepo {
    conn: Arc<Mutex<Connection>>,
}

impl HistoryRepo {
    /// 打开（或创建）历史数据库并建表。
    pub fn open(db_path: impl Into<PathBuf>) -> rusqlite::Result<Self> {
        let path = db_path.into();
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS clipboard_history (
                seq        INTEGER PRIMARY KEY,
                hash       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                text       TEXT NOT NULL,
                source     TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_history_created
                ON clipboard_history(created_at DESC);
            ",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 追加一条历史记录。
    pub fn insert(&self, entry: &HistoryEntry) -> rusqlite::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO clipboard_history (seq, hash, kind, text, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(seq) DO UPDATE SET
                hash=excluded.hash, kind=excluded.kind, text=excluded.text,
                source=excluded.source, created_at=excluded.created_at",
            rusqlite::params![
                entry.seq as i64,
                entry.hash,
                entry.kind,
                entry.text,
                entry.source,
                entry.created_at,
            ],
        )?;
        Ok(())
    }

    /// 按时间倒序分页查询历史。
    pub fn list(&self, limit: usize, before_seq: Option<u64>) -> rusqlite::Result<Vec<HistoryEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match before_seq {
            Some(_) => conn.prepare(
                "SELECT seq, hash, kind, text, source, created_at
                 FROM clipboard_history WHERE seq < ?1
                 ORDER BY seq DESC LIMIT ?2",
            )?,
            None => conn.prepare(
                "SELECT seq, hash, kind, text, source, created_at
                 FROM clipboard_history ORDER BY seq DESC LIMIT ?1",
            )?,
        };

        let rows = if let Some(seq) = before_seq {
            stmt.query_map(rusqlite::params![seq as i64, limit as i64], row_to_entry)
        } else {
            stmt.query_map(rusqlite::params![limit as i64], row_to_entry)
        }?;

        rows.collect::<Result<Vec<_>, _>>()
    }

    /// 历史总条数。
    pub fn count(&self) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM clipboard_history", [], |r| r.get(0))
    }
}

fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<HistoryEntry> {
    Ok(HistoryEntry {
        seq: row.get::<_, i64>(0)? as u64,
        hash: row.get(1)?,
        kind: row.get(2)?,
        text: row.get(3)?,
        source: row.get(4)?,
        created_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, text: &str) -> HistoryEntry {
        HistoryEntry {
            seq,
            hash: format!("hash{seq}"),
            kind: "text".to_string(),
            text: text.to_string(),
            source: Some("dev".to_string()),
            created_at: 1_000_000 + seq as i64,
        }
    }

    #[test]
    fn insert_and_list_desc() {
        let dir = tempfile::tempdir().unwrap();
        let repo = HistoryRepo::open(dir.path().join("h.db")).unwrap();
        repo.insert(&entry(1, "one")).unwrap();
        repo.insert(&entry(2, "two")).unwrap();
        repo.insert(&entry(3, "three")).unwrap();

        assert_eq!(repo.count().unwrap(), 3);

        let all = repo.list(100, None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].text, "three"); // 倒序
        assert_eq!(all[2].text, "one");

        // 分页：before_seq=3 → 返回 seq 1、2
        let page = repo.list(100, Some(3)).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].text, "two");

        // limit 生效
        let limited = repo.list(2, None).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn insert_upsert_same_seq() {
        let dir = tempfile::tempdir().unwrap();
        let repo = HistoryRepo::open(dir.path().join("h.db")).unwrap();
        repo.insert(&entry(1, "v1")).unwrap();
        repo.insert(&HistoryEntry { text: "v2".to_string(), ..entry(1, "ignored") }).unwrap();
        let all = repo.list(100, None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].text, "v2");
    }
}
