use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use domain::profile::{Profile, ProfileType};
use domain::protocol::ClipboardEvent;
use domain::storage::Storage;
use store::clipboard::ClipboardRepo;
#[cfg(feature = "history")]
use store::history::HistoryRepo;
use tokio::sync::{RwLock, broadcast};

/// 环形事件缓冲容量（Last-Event-ID 断线重连时最多补发的事件数）。
pub const EVENT_BUFFER_CAPACITY: usize = 128;

/// 剪贴板服务依赖的运行时上下文（由 server 层组装注入）。
#[derive(Clone)]
pub struct ClipboardContext {
    /// 抽象存储后端。
    pub store: Arc<dyn Storage>,
    /// 当前剪贴板内存缓存。
    pub current: Arc<RwLock<Option<Profile>>>,
    /// 变更广播（WS/SSE 订阅源），负载携带事件序号。
    pub broadcast: broadcast::Sender<ClipboardEvent>,
    /// 事件序号（单调递增，PUT 成功时 +1）。
    pub seq: Arc<AtomicU64>,
    /// 最近事件环形缓冲（供 SSE Last-Event-ID 续传）。
    pub history: Arc<RwLock<VecDeque<ClipboardEvent>>>,
    /// SQLite 历史记录仓库（feature = history 时启用）。
    #[cfg(feature = "history")]
    pub history_repo: Option<Arc<HistoryRepo>>,
}

impl ClipboardContext {
    pub fn new(store: Arc<dyn Storage>) -> Self {
        Self::with_history(store, None)
    }

    /// 携带历史仓库创建上下文。
    #[cfg(feature = "history")]
    pub fn with_history(store: Arc<dyn Storage>, history_repo: Option<Arc<HistoryRepo>>) -> Self {
        let (broadcast, _) = broadcast::channel(128);
        Self {
            store,
            current: Arc::new(RwLock::new(None)),
            broadcast,
            seq: Arc::new(AtomicU64::new(0)),
            history: Arc::new(RwLock::new(VecDeque::with_capacity(EVENT_BUFFER_CAPACITY))),
            history_repo,
        }
    }

    #[cfg(not(feature = "history"))]
    pub fn with_history<T>(store: Arc<dyn Storage>, _history_repo: Option<T>) -> Self {
        let (broadcast, _) = broadcast::channel(128);
        Self {
            store,
            current: Arc::new(RwLock::new(None)),
            broadcast,
            seq: Arc::new(AtomicU64::new(0)),
            history: Arc::new(RwLock::new(VecDeque::with_capacity(EVENT_BUFFER_CAPACITY))),
        }
    }

    /// 取下一个事件序号并追加到历史缓冲（供 Last-Event-ID 续传）。
    pub(crate) async fn record_event(&self, profile: Profile) -> ClipboardEvent {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let event = ClipboardEvent::new(seq, profile);
        let mut history = self.history.write().await;
        if history.len() >= EVENT_BUFFER_CAPACITY {
            history.pop_front();
        }
        history.push_back(event.clone());
        event
    }
}

/// 业务层错误，直接映射为 HTTP 状态码与消息。
#[derive(Debug)]
pub enum ClipboardError {
    /// 客户端提交的 profile 不合法（400）。
    Invalid(&'static str),
    /// 后端存储失败（500）。
    Storage(String),
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipboardError::Invalid(msg) => write!(f, "{msg}"),
            ClipboardError::Storage(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// PUT 结果。
#[derive(Debug)]
pub enum PutOutcome {
    /// 已持久化并广播。
    Saved(Profile),
    /// 与当前 hash 相同，幂等丢弃（未广播、未落盘）。
    Unchanged,
}

/// 剪贴板业务服务：当前值读取、上传校验、幂等持久化 + 广播。
#[derive(Default)]
pub struct ClipboardService;

impl ClipboardService {
    /// 校验 hash 是否为小写 hex SHA256（64 位 hex）。
    pub fn validate_hash(hash: &str) -> bool {
        hash.len() == 64
            && hash
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }

    /// 校验上传 profile：hash 必须是小写 hex SHA256 且与内容一致；size 与内容实际字节一致。
    pub fn validate_profile(profile: &Profile) -> Result<(), ClipboardError> {
        if !Self::validate_hash(&profile.hash) {
            return Err(ClipboardError::Invalid("hash must be lowercase hex sha256"));
        }
        match profile.r#type {
            ProfileType::Text => {
                let expected = domain::hash::text_hash(&profile.text);
                if profile.hash != expected {
                    return Err(ClipboardError::Invalid("hash does not match content"));
                }
                // size 必须等于文本 UTF-8 字节数（防虚报绕过 max_frame_size）
                if profile.size != profile.text.len() {
                    return Err(ClipboardError::Invalid("size does not match content"));
                }
            }
            ProfileType::Image => {
                // 图片：hash 必须等于 SHA256(data)，size 等于像素字节数（防虚报/损坏）
                let Some(data) = profile.data.as_ref() else {
                    return Err(ClipboardError::Invalid("image profile missing data"));
                };
                if data.is_empty() {
                    return Err(ClipboardError::Invalid("image data is empty"));
                }
                let expected = domain::hash::sha256_hex(data);
                if profile.hash != expected {
                    return Err(ClipboardError::Invalid("hash does not match content"));
                }
                if profile.size != data.len() {
                    return Err(ClipboardError::Invalid("size does not match content"));
                }
            }
            _ => {
                // 文件/多文件（group）类型 P7 预留，暂拒绝上传
                return Err(ClipboardError::Invalid("unsupported profile type"));
            }
        }
        Ok(())
    }

    /// 读取当前剪贴板（带缓存）。写锁内重查，避免并发 PUT 提交的新值被锁外补读覆盖。
    pub async fn load_current(&self, ctx: &ClipboardContext) -> Profile {
        if let Some(p) = ctx.current.read().await.clone() {
            return p;
        }
        let mut guard = ctx.current.write().await;
        if let Some(p) = guard.clone() {
            return p;
        }
        let repo = ClipboardRepo::new(ctx.store.clone());
        let profile = repo.get_current().await.unwrap_or_default();
        *guard = Some(profile.clone());
        profile
    }

    /// 上传当前剪贴板。整个读-比较-持久化-更新缓存序列在写锁内原子执行，
    /// 相同 hash 幂等丢弃（不广播、不落盘）。
    pub async fn put(
        &self,
        ctx: &ClipboardContext,
        profile: Profile,
    ) -> Result<PutOutcome, ClipboardError> {
        Self::validate_profile(&profile)?;

        let mut current_guard = ctx.current.write().await;
        let current = if let Some(c) = current_guard.clone() {
            c
        } else {
            // 缓存未初始化：写锁内补读存储，避免与并发提交互相覆盖
            let repo = ClipboardRepo::new(ctx.store.clone());
            repo.get_current().await.unwrap_or_default()
        };
        if current.hash == profile.hash {
            tracing::debug!("clipboard set idempotent, dropped (hash={})", &profile.hash);
            return Ok(PutOutcome::Unchanged);
        }

        let repo = ClipboardRepo::new(ctx.store.clone());
        repo.save_current(&profile)
            .await
            .map_err(|e| ClipboardError::Storage(e.to_string()))?;
        *current_guard = Some(profile.clone());
        let event = ctx.record_event(profile.clone()).await;
        let _ = ctx.broadcast.send(event.clone());
        tracing::info!(
            "clipboard updated (seq={}, source={:?}, type={:?}, size={})",
            event.seq,
            profile.source,
            profile.r#type,
            profile.size
        );

        // 写入 SQLite 历史（feature = history 时）
        #[cfg(feature = "history")]
        if let Some(hist) = ctx.history_repo.as_ref() {
            let kind = match profile.r#type {
                ProfileType::Text => "text",
                ProfileType::Image => "image",
                _ => "other",
            };
            let created_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let entry = store::history::HistoryEntry {
                seq: event.seq,
                hash: profile.hash.clone(),
                kind: kind.to_string(),
                text: profile.text.clone(),
                source: profile.source.clone(),
                created_at,
            };
            if let Err(e) = hist.insert(&entry) {
                tracing::error!("history insert failed: {e}");
            }
        }

        Ok(PutOutcome::Saved(profile))
    }

    /// 校验附件文件名（data_name），返回标准化的 `files/{name}` key。
    pub fn file_key(name: &str) -> Result<String, ClipboardError> {
        domain::storage::validate_file_name(name)
            .map(|_| format!("files/{name}"))
            .map_err(|_| ClipboardError::Invalid("invalid file name"))
    }

    /// 保存附件内容到 `files/{name}`。
    pub async fn put_file(
        &self,
        ctx: &ClipboardContext,
        name: &str,
        data: &[u8],
    ) -> Result<(), ClipboardError> {
        let key = Self::file_key(name)?;
        ctx.store
            .put(&key, data)
            .await
            .map_err(|e| ClipboardError::Storage(e.to_string()))
    }

    /// 读取附件内容；不存在返回 None。
    pub async fn get_file(
        &self,
        ctx: &ClipboardContext,
        name: &str,
    ) -> Result<Option<Vec<u8>>, ClipboardError> {
        let key = Self::file_key(name)?;
        ctx.store
            .get(&key)
            .await
            .map_err(|e| ClipboardError::Storage(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::local::LocalStorage;

    fn test_ctx() -> ClipboardContext {
        let store: Arc<dyn Storage> =
            Arc::new(LocalStorage::new(tempfile::tempdir().unwrap().path()));
        ClipboardContext::new(store)
    }

    fn profile(text: &str, source: Option<&str>) -> Profile {
        Profile::from_text(text.to_string(), source.map(|s| s.to_string()))
    }

    #[test]
    fn validate_hash_accepts_lowercase_hex() {
        let ok = domain::hash::text_hash("hello");
        assert!(ClipboardService::validate_hash(&ok));
        assert!(!ClipboardService::validate_hash(""));
        assert!(!ClipboardService::validate_hash("ABC"));
        assert!(!ClipboardService::validate_hash(&ok.to_uppercase()));
        assert!(!ClipboardService::validate_hash(&ok[..63]));
        assert!(!ClipboardService::validate_hash("z".repeat(64).as_str()));
    }

    #[test]
    fn validate_profile_rejects_bad_hash_and_size() {
        let mut p = profile("hello", None);
        p.hash = "a".repeat(64);
        assert!(ClipboardService::validate_profile(&p).is_err());

        let mut p = profile("hello", None);
        p.size = 0;
        assert!(ClipboardService::validate_profile(&p).is_err());

        let mut p = profile("hello", None);
        p.r#type = ProfileType::File;
        assert!(ClipboardService::validate_profile(&p).is_err());

        assert!(ClipboardService::validate_profile(&profile("hello", None)).is_ok());
    }

    #[test]
    fn validate_image_profile_accepts_valid_and_rejects_bad() {
        let data = vec![0u8; 64];
        // 合法图片 profile
        let img = Profile::from_image(data.clone(), 8, 8, Some("dev-a".to_string()));
        assert!(ClipboardService::validate_profile(&img).is_ok());

        // hash 与内容不符
        let mut bad_hash = img.clone();
        bad_hash.hash = "a".repeat(64);
        assert!(ClipboardService::validate_profile(&bad_hash).is_err());

        // size 与数据不符
        let mut bad_size = img.clone();
        bad_size.size = 1;
        assert!(ClipboardService::validate_profile(&bad_size).is_err());

        // 缺失 data 字段
        let mut no_data = img.clone();
        no_data.data = None;
        assert!(ClipboardService::validate_profile(&no_data).is_err());

        // 空数据
        let mut empty = Profile::from_image(vec![], 0, 0, None);
        empty.hash = domain::hash::sha256_hex(&[]);
        assert!(ClipboardService::validate_profile(&empty).is_err());
    }

    #[tokio::test]
    async fn put_persists_and_broadcasts() {
        let ctx = test_ctx();
        let svc = ClipboardService;
        let p = profile("你好", Some("device-a"));
        let outcome = svc.put(&ctx, p.clone()).await.unwrap();
        match outcome {
            PutOutcome::Saved(_) => {}
            PutOutcome::Unchanged => panic!("expected saved"),
        }

        let repo = ClipboardRepo::new(ctx.store.clone());
        let stored = repo.get_current().await.unwrap();
        assert_eq!(stored.hash, p.hash);

        // 广播
        let mut rx = ctx.broadcast.subscribe();
        svc.put(&ctx, profile("second", None)).await.unwrap();
        let received = rx.try_recv().unwrap();
        assert_eq!(received.profile.text, "second");
    }

    #[tokio::test]
    async fn put_same_hash_idempotent() {
        let ctx = test_ctx();
        let svc = ClipboardService;
        let p = profile("hi", None);
        assert!(matches!(
            svc.put(&ctx, p.clone()).await.unwrap(),
            PutOutcome::Saved(_)
        ));
        assert!(matches!(
            svc.put(&ctx, p.clone()).await.unwrap(),
            PutOutcome::Unchanged
        ));
    }

    #[tokio::test]
    async fn put_rejects_invalid() {
        let ctx = test_ctx();
        let svc = ClipboardService;
        let mut p = profile("hi", None);
        p.hash = "a".repeat(64);
        assert!(svc.put(&ctx, p).await.is_err());
    }

    #[tokio::test]
    async fn concurrent_puts_no_duplicate_broadcast() {
        let ctx = test_ctx();
        let svc = ClipboardService;
        svc.put(&ctx, profile("first", None)).await.unwrap();

        let mut rx = ctx.broadcast.subscribe();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let c = ctx.clone();
            let p = profile("same", None);
            handles.push(tokio::spawn(async move {
                ClipboardService.put(&c, p).await.unwrap()
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let repo = ClipboardRepo::new(ctx.store.clone());
        assert_eq!(repo.get_current().await.unwrap().text, "same");

        svc.put(&ctx, profile("later", None)).await.unwrap();
        let mut same_count = 0;
        while let Ok(msg) = rx.try_recv() {
            if msg.profile.text == "same" {
                same_count += 1;
            }
        }
        assert_eq!(
            same_count, 1,
            "concurrent same-hash PUT must broadcast exactly once"
        );
    }

    #[tokio::test]
    async fn load_current_stale_fill_does_not_clobber_new_put() {
        let ctx = test_ctx();
        let svc = ClipboardService;
        let repo = ClipboardRepo::new(ctx.store.clone());
        repo.save_current(&profile("old", None)).await.unwrap();

        let c_get = ctx.clone();
        let c_put = ctx.clone();
        let get_h = tokio::spawn(async move { ClipboardService.load_current(&c_get).await });
        let put_h =
            tokio::spawn(async move { ClipboardService.put(&c_put, profile("new", None)).await });
        put_h.await.unwrap().unwrap();
        get_h.await.unwrap();

        let cached = ctx.current.read().await.clone().unwrap();
        assert_eq!(
            cached.text, "new",
            "GET stale fill must not clobber committed PUT"
        );
        assert_eq!(svc.load_current(&ctx).await.text, "new");
    }

    #[test]
    fn file_key_validates_name() {
        assert_eq!(
            ClipboardService::file_key("photo.jpg").unwrap(),
            "files/photo.jpg"
        );
        assert!(ClipboardService::file_key("").is_err());
        assert!(ClipboardService::file_key("../evil").is_err());
        assert!(ClipboardService::file_key("a/b").is_err());
        assert!(ClipboardService::file_key(".hidden").is_err());
        assert!(ClipboardService::file_key("a\0b").is_err());
    }

    #[tokio::test]
    async fn file_put_get_roundtrip() {
        let ctx = test_ctx();
        let svc = ClipboardService;
        svc.put_file(&ctx, "photo.jpg", b"image-bytes")
            .await
            .unwrap();
        let got = svc.get_file(&ctx, "photo.jpg").await.unwrap().unwrap();
        assert_eq!(got, b"image-bytes");
        // 不存在返回 None
        assert_eq!(svc.get_file(&ctx, "missing.png").await.unwrap(), None);
        // 非法名拒绝
        assert!(svc.put_file(&ctx, "../evil", b"x").await.is_err());
    }
}
