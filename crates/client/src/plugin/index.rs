//! 插件商店索引：拉取 / 解析 / 下载校验。
//!
//! 索引为远端 YAML（`index_version: 1`，ximei.me 插件子索引），宿主只消费
//! `pluginType == clipboard_sync` 条目；.xipk 下载后先校验索引提供的 sha256，
//! 再交 [`SyncPluginManager`](super::SyncPluginManager) 安装。

use serde::Deserialize;
use sha2::Digest;
use thiserror::Error;

/// Xime 官方插件索引。
pub const DEFAULT_PLUGIN_INDEX_URL: &str = "https://index.ximei.me/plugins/index.yaml";

/// 索引解析/下载错误。
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("网络请求失败: {0}")]
    Http(#[from] reqwest::Error),
    #[error("索引解析失败: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("io 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("sha256 校验失败：期望 {expected}，实际 {actual}")]
    Sha256Mismatch { expected: String, actual: String },
}

/// 插件索引（index.yaml 根）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginIndex {
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub plugins: Vec<PluginIndexEntry>,
}

impl PluginIndex {
    /// `pluginType == clipboard_sync` 的条目（其余类型与本宿主无关）。
    pub fn clipboard_sync(&self) -> Vec<&PluginIndexEntry> {
        self.plugins
            .iter()
            .filter(|p| p.is_clipboard_sync())
            .collect()
    }
}

/// 索引条目。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginIndexEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "pluginType", default)]
    pub plugin_type: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub license: String,
    #[serde(rename = "currentVersion", default)]
    pub current_version: String,
    /// 版本列表（索引按新→旧排序）。
    #[serde(default)]
    pub versions: Vec<IndexVersion>,
}

impl PluginIndexEntry {
    pub fn is_clipboard_sync(&self) -> bool {
        self.plugin_type.eq_ignore_ascii_case("clipboard_sync")
    }

    /// 最新版本的下载信息（取首个版本的第一个下载地址）。
    pub fn latest_download(&self) -> Option<&IndexDownload> {
        self.versions.iter().flat_map(|v| v.downloads.iter()).next()
    }
}

/// 索引版本条目。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IndexVersion {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub changelog: String,
    #[serde(rename = "downloadUrl", default)]
    pub downloads: Vec<IndexDownload>,
}

/// 下载地址（含校验信息）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct IndexDownload {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(rename = "sizeBytes", default)]
    pub size_bytes: Option<u64>,
}

/// 拉取并解析插件索引。
pub async fn fetch_index(url: &str) -> Result<PluginIndex, IndexError> {
    let text = reqwest::get(url).await?.error_for_status()?.text().await?;
    parse_index(&text)
}

/// 解析索引 YAML。
pub fn parse_index(yaml: &str) -> Result<PluginIndex, IndexError> {
    Ok(serde_yaml::from_str(yaml)?)
}

/// 下载 .xipk 到临时文件；索引提供 sha256 时校验，不匹配即报错不落盘。
/// 返回临时文件路径，调用方负责清理（安装后删除）。
pub async fn download_xipk(url: &str, sha256: &str) -> Result<std::path::PathBuf, IndexError> {
    let bytes = reqwest::get(url).await?.error_for_status()?.bytes().await?;
    verify_sha256(&bytes, sha256)?;
    // 临时文件名由 URL 摘要决定：同名幂等覆盖，无需随机数依赖
    let name = format!(
        "ximed-plugin-{}.xipk",
        hex::encode(sha2::Sha256::digest(url))
    );
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, &bytes)?;
    Ok(path)
}

/// sha256 校验：期望值为空时跳过（索引个别版本未提供哈希）。
fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), IndexError> {
    let expected = expected.trim();
    if expected.is_empty() {
        return Ok(());
    }
    let actual = hex::encode(sha2::Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(IndexError::Sha256Mismatch {
            expected: expected.to_string(),
            actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实索引节选（index.ximei.me/plugins/index.yaml，2026-08）。
    const SAMPLE: &str = r#"
index_version: 1
updated_at: '2026-08-21'
plugins:
- id: com.kingzcheung.xime.plugin.kaomoji
  name: 颜文字表情包
  author: Xime
  description: 包含 174 个常用颜文字的表情插件
  type: remote
  tags:
  - 表情
  pluginType: emoji
  currentVersion: 2.1.0
  versions:
  - version: 2.1.0
    date: '2026-08-07'
    changelog: 初始发布
    downloadUrl:
    - url: https://example.com/kaomoji-2.1.0.xipk
      sha256: aaded5a9ca5511f14d60c26363ef33e11504578c9dd780d8bf30cb89df3909fc
      sizeBytes: 3297
- id: com.kingzcheung.xime.plugin.webdav_clipboard_sync
  name: WebDAV 剪贴板同步
  author: Xime
  description: 通过 WebDAV 文件同步剪贴板（文本，多设备）
  type: remote
  tags:
  - 剪贴板
  - 同步
  pluginType: clipboard_sync
  license: GPL-3.0
  currentVersion: 0.1.0
  versions:
  - version: 0.1.0
    date: '2026-08-12'
    changelog: 初始发布，Lua 插件化实现
    downloadUrl:
    - url: https://example.com/webdav-clipboard-sync-0.1.0.xipk
      sha256: 81cc872ffd0f1c6803b6617bd50e0a857de17eb538761225344b568cedbf8046
      sizeBytes: 4625
- id: com.kingzcheung.xime.plugin.ximed_sync
  name: ximed 剪贴板同步
  description: 对接 ximed HTTP 接口的双向剪贴板同步（文本）
  pluginType: clipboard_sync
  currentVersion: 0.1.0
  versions:
  - version: 0.1.0
    date: '2026-08-12'
    changelog: 初始发布
    downloadUrl:
    - url: https://example.com/ximed-clipboard-sync-0.1.0.xipk
"#;

    #[test]
    fn parses_index_and_filters_clipboard_sync() {
        let index = parse_index(SAMPLE).unwrap();
        assert_eq!(index.updated_at, "2026-08-21");
        assert_eq!(index.plugins.len(), 3);

        let sync = index.clipboard_sync();
        assert_eq!(sync.len(), 2);
        assert_eq!(
            sync[0].id,
            "com.kingzcheung.xime.plugin.webdav_clipboard_sync"
        );
        assert_eq!(sync[0].current_version, "0.1.0");
        assert_eq!(sync[0].license, "GPL-3.0");

        let dl = sync[0].latest_download().unwrap();
        assert_eq!(
            dl.url,
            "https://example.com/webdav-clipboard-sync-0.1.0.xipk"
        );
        assert_eq!(dl.size_bytes, Some(4625));
        // ximed_sync 条目无 sha256 → 空串（下载时跳过校验）
        assert!(sync[1].latest_download().unwrap().sha256.is_empty());
    }

    #[test]
    fn corrupted_index_is_error() {
        assert!(parse_index("not: [yaml").is_err());
    }

    #[test]
    fn sha256_verify() {
        let bytes = b"ximed";
        let digest = hex::encode(sha2::Sha256::digest(bytes));
        assert!(verify_sha256(bytes, &digest).is_ok());
        // 大小写不敏感
        assert!(verify_sha256(bytes, &digest.to_uppercase()).is_ok());
        // 空期望 → 跳过
        assert!(verify_sha256(bytes, "").is_ok());
        assert!(matches!(
            verify_sha256(bytes, "deadbeef"),
            Err(IndexError::Sha256Mismatch { .. })
        ));
    }
}
