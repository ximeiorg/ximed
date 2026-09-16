# PROGRESS

> 更新：2026-09-16 · 阶段：同步模型收敛（只留默认 server + 插件；删除 server WebDAV 后端与 client 直连）

## 当前状态

- **同步模型收敛（本次）**：按决策 13，删除三套 WebDAV 实现中的两套——server 端 `xime-sync-store` WebDAV 存储后端（webdav feature/WebDavStorage/build_storage 工厂，server 直用 LocalStorage，`[storage]` 配置节删除）、client 直连 WebDAV/S3 适配器（`AdapterKind::{WebDav,S3}`/`direct` 模块/SigV4/4 个 TCP mock 端到端测试）；`hmac` 依赖删除，`sha2/hex` 改为 plugin feature 可选（仅商店校验）。保留两条路径：默认 server（本机/远端 hub）与插件（云端直达，商店承载）。两个仓库均已全绿
- **上游契约迁移（上次，已提交 d9a46fa）**：schema 由插件 Lua `getSettingsSchema()` 运行时返回（上游删除 `PluginManifest.config_schema` 与 `has_fn`），ximed 侧 `config_fields`/`test_connection` 适配完成
- **同步插件框架**：复用 libximecore `xime-plugin`（.xipk 安装/Lua 沙箱/clipboard_sync 契约），ximed 侧补齐 schema 解释器、SyncPluginManager、插件商店索引
- **同步独立菜单**：侧栏「同步」页 = 同步方式（插件单选激活 + schema 动态表单 + 测试连接 + 恢复默认）+ 同步开关卡片 + 插件商店
- **ximed_sync 插件自动填充**：desktop 后台把本机 server 连接信息经 `UiMsg::LocalServerInfo` 下发；空字段自动填入（已保存值不覆盖）
- 测试全绿（client 49+1 含插件 feature 与直连删除后净量 + app 17）；fmt/clippy 干净；`--no-default-features` 最小构建不受影响

## 下一步

1. GUI 手工验证：桌面启动 ximed-desktop → 同步页刷新商店 → 安装 WebDAV 同步插件 → 选中 → 填配置 → 测试连接 → 保存应用 → 双端剪贴板同步；卸载激活插件验证回退本机 server
2. 文件同步（single_file 白名单目前仅文本/图片生效）：需 macOS/Linux/Windows 剪贴板文件路径读写（NSPasteboard / x11 / CF_HDROP），工程量最大
3. 真实多设备端到端验证（desktop 开启 server → 桌面复制文本/图片 → 另一设备同步）

## 已完成

- [x] **同步模型收敛（本次）**：server 只保留 local 后端与默认 hub/官方协议定位；client 只保留 Server/Plugin 两种适配器；删除 server 端 WebDAV 存储后端、client 直连 WebDAV/S3（含 SigV4）与 store_factory 装配层；`[storage]` 配置节与相关环境变量移除；libximecore 仓库级 reqwest 依赖删除；两仓库 cargo check/test/clippy/fmt 全绿（详见决策 13）
- [x] **上游契约迁移（上次）**：schema 运行时化（getSettingsSchema），按钮 action = key，required 透传插件声明
- [x] **同步插件框架（此前）**：plugin 模块（Lua↔Transport 桥接/registry/index）、`AdapterKind::Plugin` feature 门控分派、desktop/UI 接线
- [x] **同步设置框架**：`client::settings::SyncSettings` + 引擎遵循 + UI 卡片 + 引擎重建 + 下载通知
- [x] **图片同步**：Profile 嵌 base64 data + 引擎/服务端/UI 全链
- [x] Linux 桌面修复、三形态架构 + 发布打包 + CI（详见 git 历史）

## 待办（后续功能点）

- [ ] GUI 手工端到端验证插件同步（见下一步 1）
- [ ] 文件同步（跨平台剪贴板文件路径读写，工作量最大）
- [ ] macOS .app 打包（LSUIElement + 图标）
- [ ] 按需评估插件商店补 S3 同步插件

## 关键实现细节（本次会话）

- **收敛后的两条同步路径**：`AdapterKind::{Server,Plugin}`；默认零配置 = 本机/远端 ximed server（HTTP/WS/SSE + LocalStorage）；插件 = client 端 Lua 传输通道（云端直达），商店是唯一扩展入口；卸载激活插件自动回退本机 server
- **server 侧收缩**：`run_managed` 直用 `LocalStorage::new(data_dir)`；`StorageConfig`/`storage()` 访问器/`default_backend` 删除；default.toml `[storage]` 节删除（历史配置含 `[storage]` 的 xime.toml 不影响解析）
- **client 侧收缩**：Cargo 依赖 `hmac` 全删、`sha2/hex` 挂 `plugin` feature（商店索引 sha256 校验用）；`direct` 模块整体删除（WebDAV/S3 走插件生态，与 XimeChe 双端复用同一 .xipk）
- **schema 契约（上游 00ba2b5 起）**：schema 来自插件 Lua `getSettingsSchema()`，字段 `key/label/type/required/placeholder/helpText`；按钮 action 即按钮 key；required 透传插件声明
- **rustup 代理坑（本机开发环境）**：ZCode 沙箱改写 argv[0]，rustup 代理报 unknown proxy name；用 `~/.rustup/toolchains/stable-*/bin/{cargo,rustc}` 直连 + `RUSTC=` 环境变量绕过
- **GPG 提交**：`git commit -S`；gpg-agent 口令缓存过期后 pinentry 超时需用户重新解锁
