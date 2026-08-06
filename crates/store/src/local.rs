use std::path::PathBuf;

use async_trait::async_trait;
use domain::storage::{Result, Storage, validate_key};

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
                } else if ft.is_file()
                    && let Ok(rel) = path.strip_prefix(&self.data_dir)
                {
                    out.push(
                        rel.components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/"),
                    );
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
        store
            .put("clipboard/current.json", br#"{"type":"text"}"#)
            .await
            .unwrap();
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
        assert_eq!(
            store.get("clipboard/current.json").await.unwrap().unwrap(),
            b"v2"
        );
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        store.put("a.json", b"x").await.unwrap();
        store.delete("a.json").await.unwrap();
        store.delete("a.json").await.unwrap();
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
        assert_eq!(
            history,
            vec![
                "history/1.json".to_string(),
                "history/nested/2.json".to_string()
            ]
        );

        let missing = store.list("nope").await.unwrap();
        assert!(missing.is_empty());
    }

    #[tokio::test]
    async fn put_is_atomic_no_tmp_leftover() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        store.put("clipboard/current.json", b"data").await.unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path().join("clipboard"))
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1);
        let name = entries[0]
            .as_ref()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .to_string();
        assert_eq!(name, "current.json");
    }

    #[tokio::test]
    async fn invalid_keys_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStorage::new(dir.path());
        for bad in ["", "..", "../evil", "a/../../b", "/abs", "a\\b", "a\0b"] {
            assert!(
                store.put(bad, b"x").await.is_err(),
                "put {bad:?} should fail"
            );
            assert!(store.get(bad).await.is_err(), "get {bad:?} should fail");
        }
    }
}
