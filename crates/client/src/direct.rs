//! 直连同步适配器：绕过 ximed server，直接读写远端存储。
//!
//! 对标 SyncClipboard 的 WebDAV / S3 adapter：client 把当前剪贴板 profile
//! 序列化到远端固定键 `clipboard/current.json`（PUT/GET），轮询拉取变更。
//! 引擎的去重/回声抑制/退避逻辑完全复用（[`crate::sync::SyncEngine`]）。
//!
//! 注意：server 存储后端（store crate）保持 local 不变，直连仅发生在 client 层。

use crate::config::ClientConfig;
use crate::transport::{Transport, TransportError};

/// 远端剪贴板 profile 的存储键（WebDAV 路径 / S3 对象 key）。
pub const CLIPBOARD_KEY: &str = "clipboard/current.json";

mod s3;
mod sigv4;
mod webdav;

pub use s3::S3Transport;
pub use webdav::WebDavTransport;

use std::sync::Arc;

/// 按配置选择直连传输通道；配置不完整时返回错误。
pub fn make_direct_transport(cfg: &ClientConfig) -> Result<Arc<dyn Transport>, TransportError> {
    use crate::config::AdapterKind;
    match cfg.adapter {
        AdapterKind::WebDav => {
            if cfg.webdav_url.is_empty() {
                return Err(TransportError::Connect(
                    "webdav adapter requires [client] webdav_url".into(),
                ));
            }
            let password = cfg.webdav_password().ok_or(TransportError::AuthRejected)?;
            Ok(Arc::new(WebDavTransport::new(
                &cfg.webdav_url,
                &cfg.webdav_dir,
                &cfg.webdav_username,
                &password,
            )))
        }
        AdapterKind::S3 => {
            if cfg.s3_endpoint.is_empty() || cfg.s3_bucket.is_empty() {
                return Err(TransportError::Connect(
                    "s3 adapter requires [client] s3_endpoint and s3_bucket".into(),
                ));
            }
            let access_key = cfg.s3_access_key().ok_or(TransportError::AuthRejected)?;
            let secret_key = cfg.s3_secret_key().ok_or(TransportError::AuthRejected)?;
            Ok(Arc::new(S3Transport::new(
                &cfg.s3_endpoint,
                &cfg.s3_bucket,
                &cfg.s3_region,
                &access_key,
                &secret_key,
            )))
        }
        AdapterKind::Server | AdapterKind::Plugin => Err(TransportError::Connect(
            "server/plugin adapter is not a direct transport".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_transport_requires_config() {
        let cfg = ClientConfig::default();
        let err = make_direct_transport(&cfg).err().expect("must fail");
        assert!(matches!(err, TransportError::Connect(_)));
    }
}
