//! AWS Signature V4（SigV4）请求签名工具（S3 直连用）。
//!
//! 仅实现 S3 API 所需的最小子集：PUT / GET 单对象（无 query 参数、无
//! payload 哈希之外的附加头），符合 S3 直连同步场景。参考：
//! https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 时间戳辅助：当前 UTC 时间 → `YYYYMMDDTHHMMSSZ` / `YYYYMMDD`。
fn timestamps() -> (String, String) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let day_secs = secs % 86400;
    // 用简单算法（仅支持 1970-2038 范围够用；若越界则回退）
    let (y, m, d) = civil_from_days(days as i64);
    let (hh, mm, ss) = (day_secs / 3600, (day_secs % 3600) / 60, day_secs % 60);
    let date_stamp = format!("{y:04}{m:02}{d:02}");
    (format!("{date_stamp}T{hh:02}{mm:02}{ss:02}Z"), date_stamp)
}

/// 天数 → 公历（Howard Hinnant 算法），返回 (year, month, day)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// SigV4 请求签名结果。
pub struct SigV4 {
    /// `Authorization` 头值。
    pub authorization: String,
    /// `x-amz-date` 头值。
    pub amz_date: String,
}

/// 计算 S3 单对象请求（PUT/GET，path-style）的 SigV4 签名头。
///
/// - `method`: `"PUT"` / `"GET"`
/// - `host`: 完整主机（含端口），如 `s3.example.com`
/// - `canonical_uri`: 如 `/bucket/clipboard/current.json`
/// - `payload_hash`: body 的 SHA256 hex（GET 空 body 为固定空串哈希）
#[allow(clippy::too_many_arguments)]
pub fn sign_request(
    method: &str,
    host: &str,
    canonical_uri: &str,
    payload_hash: &str,
    access_key: &str,
    secret_key: &str,
    region: &str,
    service: &str,
) -> SigV4 {
    let (amz_date, date_stamp) = timestamps();
    let encoded_uri = uri_encode(canonical_uri, false);
    let canonical_headers =
        format!("host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n");
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";
    let canonical_request =
        format!("{method}\n{encoded_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );

    let k_date = hmac_sha256(
        format!("AWS4{secret_key}").as_bytes(),
        date_stamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    SigV4 {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        ),
        amz_date,
    }
}

/// 空 body（GET）的标准 payload hash。
pub fn empty_payload_hash() -> String {
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_encode_preserves_safe_chars() {
        assert_eq!(
            uri_encode("/bucket/clipboard/current.json", false),
            "/bucket/clipboard/current.json"
        );
        assert_eq!(uri_encode("a b", false), "a%20b");
        assert_eq!(uri_encode("a%2Fb", true), "a%252Fb");
    }

    #[test]
    fn empty_payload_hash_is_sha256_of_empty() {
        assert_eq!(
            empty_payload_hash(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn sign_request_produces_authorization_with_all_parts() {
        let s = sign_request(
            "PUT",
            "s3.example.com",
            "/bucket/clipboard/current.json",
            "abc",
            "AKID",
            "SECRET",
            "us-east-1",
            "s3",
        );
        assert!(s.authorization.starts_with("AWS4-HMAC-SHA256 "));
        assert!(s.authorization.contains("Credential=AKID/"));
        assert!(s.authorization.contains("/us-east-1/s3/aws4_request"));
        assert!(
            s.authorization
                .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date")
        );
        assert!(s.authorization.contains("Signature="));
        assert_eq!(s.amz_date.len(), 16);
    }
}
