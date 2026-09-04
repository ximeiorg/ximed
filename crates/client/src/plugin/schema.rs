//! manifest `configSchema` 解释：宿主据此渲染插件配置表单（数据驱动，无需逐插件硬编码）。
//!
//! 约定与线上 clipboard_sync 插件（Xime 仓库）一致，宽松解析：
//!
//! ```yaml
//! configSchema:
//!   - key: serverUrl        # 配置键：宿主写入 config/<id>.yaml，插件经 host.config 读取
//!     label: 服务器地址
//!     type: text            # text | secret | button；缺省/未知按 text
//!     required: false       # 缺省视为必填（线上必填字段惯常省略该键）
//!     placeholder: https://host:port
//!     helpText: 可选提示文字
//!   - key: testConnection
//!     label: 测试连接
//!     type: button
//!     action: testConnection # 按钮触发的插件契约函数
//! ```
//!
//! 配置值一律按字符串读写（`host.config` 为 String→String 映射）。

use serde::Deserialize;

/// 配置字段类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginFieldType {
    /// 单行文本。
    Text,
    /// 密文（掩码输入）。
    Secret,
    /// 动作按钮（[`PluginConfigField::action`] 指定契约函数）。
    Button,
}

/// 单个配置字段声明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConfigField {
    /// 配置键（写入 config/\<id\>.yaml）。
    pub key: String,
    /// 表单标签。
    pub label: String,
    pub field_type: PluginFieldType,
    /// 是否必填（仅作表单提示，宿主不做强制校验）。
    pub required: bool,
    /// 输入框占位文本。
    pub placeholder: String,
    /// 提示文字（helpText）。
    pub help_text: String,
    /// 按钮动作（type=button 时有效，如 `testConnection`）。
    pub action: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawField {
    #[serde(default)]
    key: String,
    #[serde(default)]
    label: String,
    #[serde(rename = "type", default)]
    field_type: String,
    #[serde(default = "default_required")]
    required: bool,
    #[serde(default)]
    placeholder: String,
    #[serde(rename = "helpText", default)]
    help_text: String,
    #[serde(default)]
    action: String,
}

fn default_required() -> bool {
    true
}

/// 解析 manifest 的 `configSchema`（宽松）：非列表或缺 key 的条目跳过，未知类型按 text。
pub fn parse_config_schema(value: &serde_yaml::Value) -> Vec<PluginConfigField> {
    let Ok(raw) = serde_yaml::from_value::<Vec<RawField>>(value.clone()) else {
        return Vec::new();
    };
    raw.into_iter()
        .filter(|f| !f.key.is_empty())
        .map(|f| {
            let label = if f.label.is_empty() {
                f.key.clone()
            } else {
                f.label
            };
            PluginConfigField {
                field_type: match f.field_type.to_lowercase().as_str() {
                    "secret" => PluginFieldType::Secret,
                    "button" => PluginFieldType::Button,
                    _ => PluginFieldType::Text,
                },
                key: f.key,
                label,
                required: f.required,
                placeholder: f.placeholder,
                help_text: f.help_text,
                action: f.action,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 线上 WebDAV 同步插件的真实 schema（节选自 0.1.0 .xipk manifest）。
    const REAL_SCHEMA: &str = r#"
- key: davUrl
  label: WebDAV 服务器
  type: text
  placeholder: https://host:port/dav/
  helpText: WebDAV 服务地址
- key: remotePath
  label: 远程目录
  type: text
  required: false
- key: username
  label: 用户名
  type: text
  required: false
- key: password
  label: 密码
  type: secret
  required: false
- key: testConnection
  label: 测试连接
  type: button
  action: testConnection
  required: false
"#;

    #[test]
    fn parses_real_world_schema() {
        let value: serde_yaml::Value = serde_yaml::from_str(REAL_SCHEMA).unwrap();
        let fields = parse_config_schema(&value);
        assert_eq!(fields.len(), 5);

        assert_eq!(fields[0].key, "davUrl");
        assert_eq!(fields[0].field_type, PluginFieldType::Text);
        // 必填字段省略 required 键 → 默认必填
        assert!(fields[0].required);
        assert_eq!(fields[0].placeholder, "https://host:port/dav/");
        assert_eq!(fields[0].help_text, "WebDAV 服务地址");

        assert!(!fields[1].required);
        assert_eq!(fields[3].field_type, PluginFieldType::Secret);

        let button = &fields[4];
        assert_eq!(button.field_type, PluginFieldType::Button);
        assert_eq!(button.action, "testConnection");
    }

    #[test]
    fn missing_or_invalid_schema_yields_empty() {
        assert!(parse_config_schema(&serde_yaml::Value::Null).is_empty());
        let scalar: serde_yaml::Value = serde_yaml::from_str("42").unwrap();
        assert!(parse_config_schema(&scalar).is_empty());
        // 条目缺 key → 跳过
        let no_key: serde_yaml::Value = serde_yaml::from_str("- label: 只有标签").unwrap();
        assert!(parse_config_schema(&no_key).is_empty());
    }

    #[test]
    fn unknown_type_falls_back_to_text_and_empty_label_uses_key() {
        let yaml: serde_yaml::Value =
            serde_yaml::from_str("- key: n\n  type: number\n  label: ''").unwrap();
        let fields = parse_config_schema(&yaml);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_type, PluginFieldType::Text);
        assert_eq!(fields[0].label, "n");
    }
}
