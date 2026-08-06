use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;

/// 存储错误。
#[derive(Debug)]
pub enum StorageError {
    /// 非法 key（路径穿越、空 key、绝对路径等）。
    InvalidKey(String),
    /// 底层 IO 错误。
    Io(std::io::Error),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::InvalidKey(key) => write!(f, "invalid storage key: {key}"),
            StorageError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, StorageError>;

/// 键值 Blob 存储抽象。所有后端（本地文件 / WebDAV / S3 / Turso）实现同一 trait。
///
/// 键空间约定（后端无关）：
/// ```text
/// clipboard/current.json   # 当前剪贴板 profile（三通道共享的唯一事实源）
/// files/{data_name}        # 附件数据（后续图片/文件）
/// history/{id}.json        # 历史记录（P7）
/// ```
#[async_trait]
pub trait Storage: Send + Sync {
    /// 写入 key → blob（覆盖）。
    async fn put(&self, key: &str, data: &[u8]) -> Result<()>;
    /// 读取 key，不存在返回 None。
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    /// 删除 key（幂等）。
    async fn delete(&self, key: &str) -> Result<()>;
    /// 列出 prefix 下的 key。
    async fn list(&self, prefix: &str) -> Result<Vec<String>>;
}

/// 校验 key 合法性：拒绝空 key、绝对路径、`..`/`.` 组件、反斜杠与 NUL 字符。
pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(StorageError::InvalidKey(key.to_string()));
    }
    if key.contains('\\') || key.contains('\0') {
        return Err(StorageError::InvalidKey(key.to_string()));
    }
    let path = Path::new(key);
    if path.is_absolute() {
        return Err(StorageError::InvalidKey(key.to_string()));
    }
    for comp in path.components() {
        match comp {
            Component::ParentDir => return Err(StorageError::InvalidKey(key.to_string())),
            Component::CurDir => return Err(StorageError::InvalidKey(key.to_string())),
            Component::RootDir => return Err(StorageError::InvalidKey(key.to_string())),
            _ => {}
        }
    }
    Ok(())
}

/// 本地文件系统存储（默认后端）。数据落在 `{data_dir}/{key}`，写入采用原子写（tmp + rename）。
#[derive(Debug, Clone)]
pub struct LocalStorage {
    data_dir: PathBuf,
}

impl LocalStorage {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    fn path_for(&self, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        Ok(self.data_dir.join(key))
    }
}

#[async_trait]
impl Storage for LocalStorage {
    async fn put(&self, key: &str, data: &[u8]) -> Result<()> {
        let path = self.path_for(key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        // 原子写：写临时文件 + rename，避免读到半截内容
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        tokio::fs::write(&tmp, data).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(key)?;
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let path = self.path_for(key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let prefix = prefix.trim_end_matches('/');
        let dir = self.data_dir.join(prefix);
        let mut out = Vec::new();
        let mut stack = vec![dir.clone()];
        while let Some(current) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&current).await {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
                Err(e) => return Err(e.into()),
            };
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let ft = match entry.file_type().await {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if ft.is_dir() {
                    stack.push(path);
                } else if ft.is_file() {
                    if let Ok(rel) = path.strip_prefix(&self.data_dir) {
                        // 还原为 "/" 分隔的 key 字符串
                        out.push(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        store.put("clipboard/current.json", br#"{"type":"text"}"#).await.unwrap();
        let data = store.get("clipboard/current.json").await.unwrap().unwrap();
        assert_eq!(data, br#"{"type":"text"}"#);
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        assert_eq!(store.get("clipboard/current.json").await.unwrap(), None);
    }

    #[tokio::test]
    async fn put_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        store.put("clipboard/current.json", b"v1").await.unwrap();
        store.put("clipboard/current.json", b"v2").await.unwrap();
        assert_eq!(store.get("clipboard/current.json").await.unwrap().unwrap(), b"v2");
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        store.put("a.json", b"x").await.unwrap();
        store.delete("a.json").await.unwrap();
        store.delete("a.json").await.unwrap(); // 第二次幂等
        assert_eq!(store.get("a.json").await.unwrap(), None);
    }

    #[tokio::test]
    async fn list_with_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        store.put("clipboard/current.json", b"c").await.unwrap();
        store.put("history/1.json", b"h1").await.unwrap();
        store.put("history/nested/2.json", b"h2").await.unwrap();
        store.put("other/x.json", b"o").await.unwrap();

        let all = store.list("").await.unwrap();
        assert_eq!(
            all,
            vec![
                "clipboard/current.json".to_string(),
                "history/1.json".to_string(),
                "history/nested/2.json".to_string(),
                "other/x.json".to_string(),
            ]
        );

        let history = store.list("history").await.unwrap();
        assert_eq!(history, vec!["history/1.json".to_string(), "history/nested/2.json".to_string()]);

        let missing = store.list("nope").await.unwrap();
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn put_is_atomic_no_tmp_leftover() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        store.put("clipboard/current.json", b"data").await.unwrap();
        // 目录里不应残留 .tmp 文件
        let entries: Vec<_> = std::fs::read_dir(dir.path().join("clipboard")).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let name = entries[0].as_ref().unwrap().file_name().to_string_lossy().to_string();
        assert_eq!(name, "current.json");
    }

    #[tokio::test]
    async fn invalid_keys_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        for bad in ["", "..", "../evil", "a/../../b", "/abs", "a\\b", "a\0b"] {
            assert!(validate_key(bad).is_err(), "key {bad:?} should be rejected");
            assert!(store.put(bad, b"x").await.is_err(), "put {bad:?} should fail");
            assert!(store.get(bad).await.is_err(), "get {bad:?} should fail");
        }
    }

    #[test]
    fn valid_keys_accepted() {
        for good in ["clipboard/current.json", "history/1.json", "files/a.txt", "a", "dir/a.b.c", ".hidden"] {
            assert!(validate_key(good).is_ok(), "key {good:?} should be accepted");
        }
    }
}
