# PROGRESS

> 更新：2026-08-07 · 阶段：同步设置框架 + 历史上限 + client 直连 + 直连本地历史 + 历史面板来源区分 + 图片同步（未提交）

## 当前状态

- **同步设置框架（SyncSettings）**：主开关默认关、push/pull 开关、上传类型白名单（text/image/single_file）、`max_file_byte`（10MB）、`notify_on_downloaded`；设置面板同步卡片；`ApplySettings` 重建引擎；下载通知回调
- **历史上限**：`HistoryRepo::open_with_limit` + insert 修剪；server `history.max_items=100`（T1 config）；handler clamp；client 跟随
- **client 直连 webdav/s3**：`AdapterKind{Server,WebDav,S3}` + `direct/{direct,webdav,s3,sigv4}.rs`（手写 SigV4）；同步目标卡片置顶；直连数据格式为单个 `clipboard/current.json`
- **直连本地历史**：`client::history_store::LocalHistory`（复用 store::HistoryRepo，seq=微秒时间戳）+ `SyncEngine::set_history_handler`；desktop/client-only 历史轮询统一读本地 `client-history.db`
- **同步模型**：跨设备只同步"当前焦点"（单键 current），本地保存完整历史（含 source）；`self_written` 防回环
- **历史面板来源区分**：本机记录灰 tag「本机」；远程来源强调色 tag「远程 · 设备」
- **图片同步（本次）**：Profile 内嵌 base64 data/width/height；ClipboardAdapter 增 `get_image/set_image`（arboard）；引擎图片优先轮询 + 远端 image 写剪贴板；server 校验 image 并存储；**端到端实测通过**（dev-a 推 1920x1080 截图 → server 存储 → dev-b 启动采纳写入本地剪贴板）
- **125 个测试全绿**；fmt 干净；clippy 无项目警告；`cargo check -p app --no-default-features` 通过

## 下一步

1. 审查未提交变更后提交
2. 文件同步（single_file 白名单目前仅文本/图片生效）：需 macOS/Linux/Windows 剪贴板文件路径读写（NSPasteboard / x11 / CF_HDROP），工程量最大
3. 真实多设备端到端验证（desktop 开启 server → 桌面复制文本/图片 → 另一设备同步）

## 已完成

- [x] **同步设置框架**：`client::settings::SyncSettings` + 引擎遵循（主开关/推送/拉取/白名单/大小/通知）+ UI 卡片 + 引擎重建 + 下载通知
- [x] **历史上限 100**：store 修剪 SQL（`insert_prunes_beyond_max_items`、`prune_keeps_upserted_seq`）
- [x] **client 直连**：WebDAV（PUT/GET current.json + Basic Auth）、S3（手写 SigV4，仅 hmac/sha2/hex）、4 个 TCP mock 端到端测试；删除过时 `docs/design/clipboard-sync.md`
- [x] **直连本地历史**：`history_store.rs`（record 写 source）、Push 成功/Write 剪贴板时记录、3s 轮询发 `UiMsg::History`、server 适配器不再 HTTP 拉历史作为 UI 来源
- [x] **历史来源视觉区分**：`history_tag_remote` 强调色 tag；远程「远程 · 设备名」/本机「本机」；移除 desktop `HttpTransport` import
- [x] **图片同步（本次）**：
  - domain：Profile 增 `data`（base64 序列化，`skip_serializing_if none` 保持旧 JSON 兼容）/`width`/`height` + `from_image`（hash=SHA256(data)）
  - client clipboard：`ImageData{width,height,bytes}` + `get_image/set_image`（arboard，worker 线程可用）
  - client sync：图片优先轮询 `on_image_poll`、`SyncAction::WriteImage`、`apply_remote` 支持 Image、startup_pull 支持图片并**执行采纳 action**（修复：原只决策不执行，文本分支 WS 广播掩盖）
  - server：`default.toml`/serde default `max_frame_size` 1MB→16MB（对齐 10MB 上限 + base64 膨胀）；`service::ClipboardService.validate_profile` 支持 Image（hash/size/data 校验）；历史 kind 增 "image"
  - UI：历史卡片 image 类型显示「[图片]」占位；空态文案更新
  - 直连集成测试增 webdav/s3 图片往返；client 引擎增 8 个图片测试；service 增 image 校验测试
  - 端到端实测：截屏 → daemon client 推送（server `type:image, 1920x1080, source`）→ 另一 client 启动采纳写入剪贴板 ✓
- [x] 图标修复：librsvg 渲染透明 PNG + 重新生成 icns/ico；侧栏真实图标（iced image + include_bytes）
- [x] 三形态架构 + 发布打包 + CI（见下）

## 待办（后续功能点）

- [ ] 审查未提交变更后 GPG 提交
- [ ] 文件同步（跨平台剪贴板文件路径读写，工作量最大）
- [ ] macOS .app 打包（LSUIElement + 图标）
- [ ] S3 存储后端（按需，大依赖）

## 关键实现细节（本次会话）

- **图片载荷**：Profile 内嵌 base64 `data`（`serde with` 自定义序列化，避免数字数组）；传输/server/webdav/s3/历史存储零改动，仅 server body limit 与校验放开
- **server body limit 陷阱**：`MemorySource` 内嵌 `default.toml`（include_str!）优先级高于 serde default fn，改默认值必须改 `default.toml` 而非 `#[serde(default)]`；axum DefaultBodyLimit 拒绝超限 body（413）
- **引擎 startup_pull bug**：决策（apply_remote）后未 `execute`，启动采纳从未写入剪贴板；文本场景被 WS next_event 广播掩盖，图片场景暴露；已修复并补回归测试
- **macOS 剪贴板**：arboard `get_image/set_image` 在 tokio worker 线程可用（无主线程依赖）；`screencapture -x -c` 截图可被 arboard 读取（RGBA8）
- 直连凭据：环境变量优先，设置面板明文字段兜底；避免 `unsafe set_var`（Rust 2024）
- SigV4 手写：`host;x-amz-content-sha256;x-amz-date` 签署；`#[allow(clippy::too_many_arguments)]`
