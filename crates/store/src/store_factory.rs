use std::sync::Arc;

use domain::storage::Storage;

use crate::local::LocalStorage;
#[cfg(feature = "webdav")]
use crate::webdav::WebDavStorage;

/// 后端类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Local,
    #[cfg(feature = "webdav")]
    WebDav,
}

impl BackendKind {
    /// 从配置字符串解析后端类型，未知值回退 local。
    pub fn from_name(name: &str) -> Self {
        match name {
            #[cfg(feature = "webdav")]
            "webdav" => BackendKind::WebDav,
            _ => BackendKind::Local,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            BackendKind::Local => "local",
            #[cfg(feature = "webdav")]
            BackendKind::WebDav => "webdav",
        }
    }
}

/// 存储后端装配参数（server 层从配置填充）。
pub struct StorageOptions {
    /// 本地后端数据目录。
    pub data_dir: String,
    /// WebDAV 后端地址。
    pub webdav_url: Option<String>,
    /// WebDAV 用户名。
    pub webdav_username: Option<String>,
    /// WebDAV 密码。
    pub webdav_password: Option<String>,
}

impl StorageOptions {
    /// 仅本地后端（测试/默认路径）。
    pub fn local(data_dir: impl Into<String>) -> Self {
        Self {
            data_dir: data_dir.into(),
            webdav_url: None,
            webdav_username: None,
            webdav_password: None,
        }
    }
}

/// 按后端类型构建存储实例。
pub fn build_storage(kind: BackendKind, opts: &StorageOptions) -> Arc<dyn Storage> {
    match kind {
        BackendKind::Local => Arc::new(LocalStorage::new(&opts.data_dir)),
        #[cfg(feature = "webdav")]
        BackendKind::WebDav => {
            let url = opts.webdav_url.clone().unwrap_or_default();
            Arc::new(WebDavStorage::new(
                url,
                opts.webdav_username.clone(),
                opts.webdav_password.clone(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_parse() {
        assert_eq!(BackendKind::from_name("local"), BackendKind::Local);
        assert_eq!(BackendKind::from_name("unknown"), BackendKind::Local);
        assert_eq!(BackendKind::Local.as_str(), "local");
        #[cfg(feature = "webdav")]
        {
            assert_eq!(BackendKind::from_name("webdav"), BackendKind::WebDav);
            assert_eq!(BackendKind::WebDav.as_str(), "webdav");
        }
    }
}
