use std::sync::Arc;

use domain::profile::Profile;
use domain::storage::{Storage, StorageError};

/// 当前剪贴板 profile 的存储 key（三通道共享的唯一事实源）。
pub const CURRENT_KEY: &str = "clipboard/current.json";

/// 剪贴板数据存取仓库：把「当前剪贴板」的落盘/读取细节（key、JSON 序列化）
/// 从业务逻辑中隔离。service 层只与 Profile 交互，不感知存储格式。
pub struct ClipboardRepo {
    store: Arc<dyn Storage>,
}

impl ClipboardRepo {
    pub fn new(store: Arc<dyn Storage>) -> Self {
        Self { store }
    }

    /// 读取当前剪贴板 profile；不存在或损坏返回 None。
    pub async fn get_current(&self) -> Option<Profile> {
        match self.store.get(CURRENT_KEY).await {
            Ok(Some(bytes)) => serde_json::from_slice::<Profile>(&bytes).ok(),
            _ => None,
        }
    }

    /// 覆盖写入当前剪贴板 profile（原子写由后端保证）。
    pub async fn save_current(&self, profile: &Profile) -> Result<(), StorageError> {
        let bytes =
            serde_json::to_vec(profile).map_err(|e| StorageError::Io(std::io::Error::other(e)))?;
        self.store.put(CURRENT_KEY, &bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::LocalStorage;

    #[tokio::test]
    async fn repo_roundtrip() {
        let store: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tempfile::tempdir().unwrap().path()));
        let repo = ClipboardRepo::new(store.clone());

        // 初始无数据
        assert_eq!(repo.get_current().await, None);

        let p = Profile::from_text("你好", Some("d1".to_string()));
        repo.save_current(&p).await.unwrap();

        let got = repo.get_current().await.unwrap();
        assert_eq!(got.hash, p.hash);
        assert_eq!(got.text, "你好");
    }

    #[tokio::test]
    async fn repo_corrupted_file_returns_none() {
        let store: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tempfile::tempdir().unwrap().path()));
        store.put(CURRENT_KEY, b"not-json{{").await.unwrap();
        let repo = ClipboardRepo::new(store);
        assert_eq!(repo.get_current().await, None);
    }
}
