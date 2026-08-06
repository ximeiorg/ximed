use serde::{Deserialize, Serialize};

/// 剪贴板数据类型。枚举值序列化为小写（`"type": "text"`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileType {
    Text,
    File,
    Image,
    Group,
    None,
}

/// 剪贴板条目（Profile）。所有字段一律小写 snake_case。
///
/// JSON 示例：
/// ```json
/// {
///   "type": "text",
///   "hash": "9f86d081884c7d65...",
///   "text": "完整文本内容",
///   "has_data": false,
///   "data_name": null,
///   "size": 12,
///   "source": "device-a"
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub r#type: ProfileType,
    /// 小写 hex SHA256。text 类型 = SHA256(utf8(text))。
    pub hash: String,
    /// 文本内容或预览。
    pub text: String,
    /// 是否有附件数据文件（图片/文件）。
    pub has_data: bool,
    /// 附件文件名（后续图片/文件同步使用）。
    pub data_name: Option<String>,
    /// 内容字节大小。
    pub size: usize,
    /// 来源设备标识。
    pub source: Option<String>,
}

impl Profile {
    /// 由文本构造一个 text 类型 profile，自动计算 hash 与 size。
    pub fn from_text(text: impl Into<String>, source: Option<String>) -> Self {
        let text = text.into();
        let hash = crate::hash::text_hash(&text);
        let size = text.len();
        Self {
            r#type: ProfileType::Text,
            hash,
            text,
            has_data: false,
            data_name: None,
            size,
            source,
        }
    }

    /// 空剪贴板初始值（配置 `clipboard.default_profile` 语义）。
    pub fn empty(source: Option<String>) -> Self {
        Self {
            r#type: ProfileType::Text,
            hash: String::new(),
            text: String::new(),
            has_data: false,
            data_name: None,
            size: 0,
            source,
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::empty(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_lowercase_fields() {
        let p = Profile::from_text("你好", Some("device-a".to_string()));
        let json = serde_json::to_string(&p).unwrap();
        // 字段一律小写：type / has_data / data_name / source，不允许大写开头
        for field in [
            "type",
            "hash",
            "text",
            "has_data",
            "data_name",
            "size",
            "source",
        ] {
            assert!(
                json.contains(&format!("\"{field}\"")),
                "missing field {field}: {json}"
            );
        }
        for bad in ["Type", "HasData", "DataName", "Source"] {
            assert!(!json.contains(bad), "uppercase field {bad} in {json}");
        }
        assert!(
            json.contains("\"type\":\"text\""),
            "enum should serialize lowercase: {json}"
        );
    }

    #[test]
    fn deserialize_lowercase_fields() {
        let json = r#"{
            "type": "text",
            "hash": "abc",
            "text": "hi",
            "has_data": false,
            "data_name": null,
            "size": 2,
            "source": "device-a"
        }"#;
        let p: Profile = serde_json::from_str(json).unwrap();
        assert_eq!(p.r#type, ProfileType::Text);
        assert_eq!(p.text, "hi");
        assert_eq!(p.size, 2);
        assert_eq!(p.source.as_deref(), Some("device-a"));
        assert!(!p.has_data);
    }

    #[test]
    fn from_text_computes_hash_and_size() {
        let p = Profile::from_text("hello", None);
        assert_eq!(p.r#type, ProfileType::Text);
        assert_eq!(p.hash, crate::hash::text_hash("hello"));
        assert_eq!(p.size, 5);
        assert!(!p.has_data);
        assert_eq!(p.data_name, None);
    }

    #[test]
    fn profile_type_roundtrip_all_variants() {
        for (ty, s) in [
            (ProfileType::Text, "text"),
            (ProfileType::File, "file"),
            (ProfileType::Image, "image"),
            (ProfileType::Group, "group"),
            (ProfileType::None, "none"),
        ] {
            let json = serde_json::to_string(&ty).unwrap();
            assert_eq!(
                json,
                format!("\"{s}\""),
                "variant {s} should serialize lowercase"
            );
            let back: ProfileType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ty);
        }
    }

    #[test]
    fn empty_profile() {
        let p = Profile::empty(None);
        assert_eq!(p.hash, "");
        assert_eq!(p.text, "");
        assert_eq!(p.size, 0);
        assert!(!p.has_data);
        assert_eq!(Profile::default(), p);
    }
}
