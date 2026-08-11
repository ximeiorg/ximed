# PROGRESS

> 更新：2026-08-11 · 阶段：Linux 桌面修复（tray GTK 初始化 + 任务栏图标 + 状态显示局域网 IP + Wayland 剪贴板 + 同步主开关默认开）

## 当前状态

- **Linux 托盘修复**：muda/tray-icon 的 gtk 后端要求先 `gtk::init()`（iced 不初始化 GTK）；headless 无显示环境优雅降级跳过托盘
- **Linux 任务栏图标**：iced `application_id = "ximed-desktop"`（与 cargo-packager 生成的 `ximed-desktop.desktop` 匹配；GNOME 等按 app_id 匹配 .desktop 取图标，忽略窗口图标）
- **状态面板局域网地址**：server 启动时 UDP 探测默认路由 IP，`listening on 0.0.0.0:4455 · 局域网 192.168.1.5:4455`
- **Wayland 剪贴板修复**：arboard 启用 `wayland-data-control`（wlr-data-control 直连，KDE Wayland 无 klipper 时 XWayland 读不到 Wayland 原生复制内容——已实测复现并修复）
- **同步主开关默认开启**：`SyncSettings::default().switch_on = true`（新安装开箱即用）
- 125+ 测试全绿（client 42 + app 14 + 其余）；fmt/clippy 干净

## 下一步

1. 审查未提交变更后提交（GPG 签名）
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

- [x] Linux 桌面修复（本次）：tray GTK 初始化 + application_id 图标 + 状态局域网 IP + Wayland 剪贴板 + 同步主开关默认开
- [ ] 文件同步（跨平台剪贴板文件路径读写，工作量最大）
- [ ] macOS .app 打包（LSUIElement + 图标）
- [ ] S3 存储后端（按需，大依赖）

## 关键实现细节（本次会话）

- **Linux 托盘 GTK 初始化**：muda/tray-icon 的 gtk 后端在 `Menu::new()` 时 panic「GTK has not been initialized」；Linux 下建托盘前 `gtk::init()`，headless（无显示）时 init 失败返回 Err，由 `ensure_tray` 记录 warning 跳过托盘
- **Linux 任务栏图标机制**：GNOME/KDE 等桌面不读窗口图标（NET_WM_ICON/Wayland 无窗口图标概念），按 app_id（Wayland）/WM_CLASS（X11）匹配 `/usr/share/applications/<id>.desktop` 取 `Icon=`；cargo-packager 的 deb/appimage 自动生成 `{main_binary}.desktop`（Icon=main_binary 名、hicolor 图标）→ app_id 必须设为 `ximed-desktop`
- **Wayland 剪贴板坑**：arboard 默认无 `wayland-data-control` feature，Wayland 会话回退 XWayland X11 通道；KDE 的 XWayland↔Wayland 剪贴板桥接由 klipper 承担，未运行 klipper 时读剪贴板恒为 `ContentNotAvailable`（引擎永远检测不到复制）。启用 `wlr-data-control` 直连真实剪贴板
- **同步主开关默认关的坑**：desktop 形态 `SyncSettings::switch_on` 默认 false 时引擎空转（不推不拉、历史为空），用户以为坏了；改为默认开启
- **局域网 IP 探测**：UDP connect（`bind 0.0.0.0:0` + `connect 8.8.8.8:80`）取默认路由 IP，零新依赖；`rsplit_once(':')` 兼容 IPv6 监听地址取端口
- 图片载荷：Profile 内嵌 base64 `data`（`serde with` 自定义序列化，避免数字数组）；传输/server/webdav/s3/历史存储零改动，仅 server body limit 与校验放开
- **server body limit 陷阱**：`MemorySource` 内嵌 `default.toml`（include_str!）优先级高于 serde default fn，改默认值必须改 `default.toml` 而非 `#[serde(default)]`；axum DefaultBodyLimit 拒绝超限 body（413）
- **引擎 startup_pull bug**：决策（apply_remote）后未 `execute`，启动采纳从未写入剪贴板；文本场景被 WS next_event 广播掩盖，图片场景暴露；已修复并补回归测试
- **macOS 剪贴板**：arboard `get_image/set_image` 在 tokio worker 线程可用（无主线程依赖）；`screencapture -x -c` 截图可被 arboard 读取（RGBA8）
- 直连凭据：环境变量优先，设置面板明文字段兜底；避免 `unsafe set_var`（Rust 2024）
- SigV4 手写：`host;x-amz-content-sha256;x-amz-date` 签署；`#[allow(clippy::too_many_arguments)]`
