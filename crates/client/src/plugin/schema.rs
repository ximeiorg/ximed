//! `getSettingsSchema` 契约解释：宿主据此渲染插件配置表单（数据驱动，无需逐插件硬编码）。
//!
//! schema 由插件 Lua 入口的 `getSettingsSchema()` 返回（上游 xime-plugin 已移除
//! manifest `configSchema` 字段），经 [`PluginRuntime::get_settings_schema`] 落到本模块转换。
//! 约定与 XimeChe 一致，宽松解析：
//!
//! ```lua
//! function plugin.getSettingsSchema()
//!     return {
//!         { key = "serverUrl", label = "服务器地址", type = "text",          -- text | secret | button
//!           required = false, placeholder = "https://host:port",
//!           helpText = "可选提示文字" },
//!         { key = "testConnection", label = "测试连接", type = "button" },    -- 按钮即调 key 同名契约函数
//!     }
//! end
//! ```
//!
//! 配置值一律按字符串读写（`host.config` 为 String→String 映射）。

use xime_plugin::SettingField;

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

/// 转换插件运行时 schema（宽松）：缺 key 的条目由上游丢弃，未知类型按 text。
pub fn parse_settings_schema(schema: &[SettingField]) -> Vec<PluginConfigField> {
    schema
        .iter()
        .filter(|f| !f.key.is_empty())
        .map(|f| {
            let label = if f.label.is_empty() {
                f.key.clone()
            } else {
                f.label.clone()
            };
            let ftype = f.ftype.to_lowercase();
            PluginConfigField {
                field_type: match ftype.as_str() {
                    "secret" => PluginFieldType::Secret,
                    "button" => PluginFieldType::Button,
                    _ => PluginFieldType::Text,
                },
                key: f.key.clone(),
                label,
                required: f.required,
                placeholder: f.placeholder.clone().unwrap_or_default(),
                help_text: f.help_text.clone().unwrap_or_default(),
                // 按钮字段无独立 action 键：契约约定按钮的 key 即所调函数名
                action: if ftype == "button" {
                    f.key.clone()
                } else {
                    String::new()
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 线上 WebDAV 同步插件的真实 schema（节选自 0.1.0 .xipk `getSettingsSchema` 返回）。
    fn real_schema() -> Vec<SettingField> {
        vec![
            SettingField {
                key: "davUrl".into(),
                label: "WebDAV 服务器".into(),
                ftype: "text".into(),
                placeholder: Some("https://host:port/dav/".into()),
                help_text: Some("WebDAV 服务地址".into()),
                required: false,
                ..Default::default()
            },
            SettingField {
                key: "remotePath".into(),
                label: "远程目录".into(),
                ftype: "text".into(),
                required: false,
                ..Default::default()
            },
            SettingField {
                key: "username".into(),
                label: "用户名".into(),
                ftype: "text".into(),
                required: false,
                ..Default::default()
            },
            SettingField {
                key: "password".into(),
                label: "密码".into(),
                ftype: "secret".into(),
                required: false,
                ..Default::default()
            },
            SettingField {
                key: "testConnection".into(),
                label: "测试连接".into(),
                ftype: "button".into(),
                required: false,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn parses_real_world_schema() {
        let fields = parse_settings_schema(&real_schema());
        assert_eq!(fields.len(), 5);

        assert_eq!(fields[0].key, "davUrl");
        assert_eq!(fields[0].field_type, PluginFieldType::Text);
        // required 直接透传插件声明（插件未声明时上游视为非必填，区别于旧的 manifest 缺省必填约定）
        assert!(!fields[0].required);
        assert_eq!(fields[0].placeholder, "https://host:port/dav/");
        assert_eq!(fields[0].help_text, "WebDAV 服务地址");

        assert!(!fields[1].required);
        assert_eq!(fields[3].field_type, PluginFieldType::Secret);

        let button = &fields[4];
        assert_eq!(button.field_type, PluginFieldType::Button);
        // 按钮即调 key 同名契约函数
        assert_eq!(button.action, "testConnection");
    }

    #[test]
    fn empty_schema_yields_empty() {
        assert!(parse_settings_schema(&[]).is_empty());
    }

    #[test]
    fn unknown_type_falls_back_to_text_and_empty_label_uses_key() {
        let fields = parse_settings_schema(&[SettingField {
            key: "n".into(),
            label: String::new(),
            ftype: "number".into(),
            ..Default::default()
        }]);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_type, PluginFieldType::Text);
        assert_eq!(fields[0].label, "n");
    }
}
