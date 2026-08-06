//! 剪贴板历史条目（与服务端 GET /api/clipboard/history 返回结构一致）。

use serde::Deserialize;

/// 单条剪贴板历史记录。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HistoryItem {
    /// 事件序号（与广播 seq 一致）。
    pub seq: u64,
    /// 内容 hash。
    pub hash: String,
    /// 类型（text/file/image/...）。
    #[serde(rename = "type")]
    pub kind: String,
    /// 文本内容或预览。
    pub text: String,
    /// 来源设备。
    pub source: Option<String>,
    /// 记录时间（Unix 秒）。
    pub created_at: i64,
}

/// 历史接口响应包装。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HistoryResponse {
    pub items: Vec<HistoryItem>,
    #[serde(default)]
    pub count: usize,
}
