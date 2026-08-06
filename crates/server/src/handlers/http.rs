use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use domain::profile::Profile;
use service::{ClipboardError, ClipboardService, PutOutcome};

use crate::auth::check_basic_auth;
use crate::state::SharedState;

/// 认证中间件式检查：失败返回 401 响应。
pub async fn require_auth(headers: &HeaderMap, state: &SharedState) -> Result<(), Response> {
    if check_basic_auth(headers, &state.auth) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response())
    }
}

/// GET /healthz
pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

/// GET /api/version
pub async fn version() -> Json<serde_json::Value> {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") }))
}

/// GET /api/clipboard
/// 返回当前剪贴板 profile；带 `If-None-Match: <本地hash>` 且 hash 一致时返回 304。
pub async fn get_clipboard(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Response {
    if let Err(resp) = require_auth(&headers, &state).await {
        return resp;
    }
    let profile = ClipboardService.load_current(&state.clipboard).await;
    let profile_hash = profile.hash.clone();

    if let Some(etag) = headers.get(axum::http::header::IF_NONE_MATCH) {
        if let Ok(etag) = etag.to_str() {
            let etag = etag.trim_matches('"');
            if etag == profile_hash {
                return StatusCode::NOT_MODIFIED.into_response();
            }
        }
    }

    let mut resp = Json(profile).into_response();
    if !profile_hash.is_empty() {
        // hash 理论上已在 PUT 侧校验为 hex，此处仍容错避免非法值导致 panic
        if let Ok(header) = HeaderValue::from_str(&format!("\"{}\"", profile_hash)) {
            resp.headers_mut().insert(axum::http::header::ETAG, header);
        }
    }
    resp
}

/// PUT /api/clipboard
/// 上传 profile（覆盖当前剪贴板）。与当前 hash 相同则幂等丢弃（不广播、不落盘）。
pub async fn put_clipboard(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(profile): Json<Profile>,
) -> Response {
    if let Err(resp) = require_auth(&headers, &state).await {
        return resp;
    }
    match ClipboardService.put(&state.clipboard, profile).await {
        Ok(PutOutcome::Saved(p)) => Json(json!({ "ok": true, "hash": p.hash })).into_response(),
        Ok(PutOutcome::Unchanged) => Json(json!({ "ok": true, "unchanged": true })).into_response(),
        Err(ClipboardError::Invalid(msg)) => {
            (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response()
        }
        Err(ClipboardError::Storage(msg)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": msg }))).into_response()
        }
    }
}

/// 返回带 body 限制的 layer（在 main.rs 装配时调用）。
/// axum 默认 body 上限 2MB，这里用配置的 max_frame_size 收紧到链路层。
pub fn body_limit(max_frame_size: usize) -> DefaultBodyLimit {
    DefaultBodyLimit::max(max_frame_size)
}

/// GET /api/clipboard/history?limit=50&before_seq=123
/// 返回剪贴板历史（时间倒序，分页）。history 未启用或失败返回空列表。
pub async fn get_history(
    State(state): State<SharedState>,
    headers: HeaderMap,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(resp) = require_auth(&headers, &state).await {
        return resp;
    }
    let limit = query
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(50)
        .clamp(1, 200);
    let before_seq = query.get("before_seq").and_then(|s| s.parse::<u64>().ok());

    #[cfg(feature = "history")]
    {
        if let Some(repo) = &state.clipboard.history_repo {
            let entries = repo.list(limit, before_seq).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("history query failed: {e}") })),
                )
                    .into_response()
            });
            match entries {
                Ok(list) => {
                    let items: Vec<_> = list
                        .iter()
                        .map(|e| {
                            json!({
                                "seq": e.seq,
                                "hash": e.hash,
                                "type": e.kind,
                                "text": e.text,
                                "source": e.source,
                                "created_at": e.created_at,
                            })
                        })
                        .collect();
                    return Json(json!({ "items": items, "count": items.len() })).into_response();
                }
                Err(resp) => return resp,
            }
        }
    }
    // 未启用 history 或仓库缺失：返回空
    Json(json!({ "items": [], "count": 0 })).into_response()
}

/// GET /api/clipboard/file?name=photo.jpg
/// 下载附件内容（Content-Type: application/octet-stream）。
pub async fn get_file(
    State(state): State<SharedState>,
    headers: HeaderMap,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(resp) = require_auth(&headers, &state).await {
        return resp;
    }
    let Some(name) = query.get("name") else {
        return (StatusCode::BAD_REQUEST, "missing name").into_response();
    };
    match ClipboardService.get_file(&state.clipboard, name).await {
        Ok(Some(bytes)) => {
            let mut resp = Response::new(axum::body::Body::from(bytes));
            resp.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".parse().unwrap(),
            );
            resp
        }
        Ok(None) => (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(ClipboardError::Invalid(msg)) => {
            (StatusCode::BAD_REQUEST, msg).into_response()
        }
        Err(ClipboardError::Storage(msg)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
        }
    }
}
