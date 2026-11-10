# PROGRESS

> 更新：2026-11-10 · 阶段：桌面重构 R2/R3 完成（五页信息架构 + 三通道接线 + P2P 配对）

## 当前状态

- **R3 局域网直连接线 + 配对页（本次）**：`P2pTransport`（`push`→`set_clipboard`、`pull`→
  `Ok(None)`、`next_event`→非阻塞 `poll` 轮询）+ `client::p2p::{start_display_pairing,
  start_join_pairing, remove_peer, local_port}`。配对前 `shutdown()` 让出 47821/47822
  端口、结束后按原状态重启（配对与常驻节点抢同一组端口，且监听都没 `SO_REUSEADDR`）。
  设备页 = 本机身份 / 配对（分段控件「显示码 / 输入码」+ 60 秒倒计时进度条）/ 已配对设备
- **R2 桌面界面重构（本次）**：侧栏五页（概览 / 同步通道 / 设备 / 历史 / 设置）。
  `UiState` 的 20 多个平行字段收敛成一份 `AppSettings` 草稿（编辑态与已保存态分离）；
  `Panel::{Status,Sync,Device,History,Settings}`；视觉组件对齐
  `libximecore/xime-setup`：`settings_group` / `settings_item` / `tab_bar` / `badge` /
  `banner` / 主题化 `toggler` / 卡片投影 / 统一控件高度；侧栏改成分组式
- **R2a 通道模型与配置迁移（本次）**：`Channel` 收敛为 http/webdav/p2p 三个内置通道
  （删除插件残留变体与宽松解析）；新增 `client/src/router.rs`（`Router` 实现 `Transport`，
  每轮按 `channel::decide` 复核并切换，切换对引擎透明）；`make_transport` 按
  `channels` 列表装配、跳过不可用通道、全空退回 HTTP。`AppSettings` 改为
  `channels` + `[http]` + `[webdav]` + `[p2p]` + `[sync]`，旧扁平 `mode`/`remote_*`
  自动迁移，未知通道/空列表规范化
- **插件体系移除（上次，1646360，决策 16）**：client 插件模块/feature/依赖、`AdapterKind`、
  桌面插件 UI 与 8 个消息全部删除；旧配置字段被 serde 忽略
- **R1 WebDAV 原生通道（5a7a20f）**：`client/src/webdav.rs`（PUT/GET 单文件 +
  `test_connection` PROPFIND 打目录），5 项测试含自建 mock WebDAV 端到端
- **P1 设备层（d520929）**：`client/src/p2p.rs` 节点进程级单例 + `<base>/p2p` 数据目录
  + 连接态 + 已配对设备读取（表损坏报错、不回落空表）
- **P0 依赖迁移（fdf929a）**：domain 迁 libximep2p（pin `b19ecde`）+ patch 拆段

验证：`cargo check --workspace`（默认 / `--no-default-features`）双绿；
`cargo test --workspace` 70 项通过（client 54 + app 16）；clippy/fmt 干净

## 下一步

1. **GUI 手工验证三条通道**：WebDAV（地址/账号 → 测试连接 → 双端同步）、局域网直连
   （两台设备同一网络 → 生成码/输入码 → 复制文本自动同步）、HTTP（概览页启停 server）
2. **配对端口的长期方案**：当前靠「配对前暂停节点」绕开端口冲突，代价是配对期间同步
   中断。上游给 `Watcher::bind` 加 `SO_REUSEADDR`（或配对改用临时端口 + 公告真实端口）
   后可去掉暂停逻辑
3. 文件同步（跨平台剪贴板文件路径读写，工程量最大）
4. macOS .app 打包（LSUIElement + 图标）
5. **LICENSE 待决**：`xime-p2p` / `xime-sync-domain` 均为 GPL-3.0-or-later，而桌面形态
   现在默认编入 p2p（`tray` feature 依赖 `client/p2p`），默认产物已是 GPL 代码——
   需要把本仓 LICENSE 整体转 GPL-3.0-or-later（与 XimeYi / Android 一致）

## 已完成

- [x] **R2/R3 桌面重构（本次）**：五页 IA + 通道模型/配置迁移 + `Router` 降级 + P2P 配对页；`UiState` 改单一草稿；视觉对齐 xime-setup 组件（详见决策 17~19）
- [x] **插件体系移除（决策 16）**：client 插件模块/feature/依赖、AdapterKind、app 插件 UI 与 8 个消息全部删除；旧配置字段被 serde 忽略、回落 HTTP 通道；59 项测试通过
- [x] **P1 局域网 P2P 设备层（d520929）**：`client/src/p2p.rs` 节点单例 + 数据目录 + 连接态 + 已配对设备读取；feature `p2p` 默认关；`cargo test -p client --features p2p` 4 项通过（详见决策 15）
- [x] **P0 局域网 P2P 依赖迁移（上次，fdf929a）**：domain 迁 libximep2p（pin `b19ecde`）+ patch 拆段 + xime-plugin pin `bdaff98` + config 下限 0.15.27；两种构建形态 check 双绿、50 项测试全过、fmt 干净（详见决策 14）
- [x] **同步模型收敛（上次）**：server 只保留 local 后端与默认 hub/官方协议定位；client 只保留 Server/Plugin 两种适配器；删除 server 端 WebDAV 存储后端、client 直连 WebDAV/S3（含 SigV4）与 store_factory 装配层；`[storage]` 配置节与相关环境变量移除；libximecore 仓库级 reqwest 依赖删除；两仓库 cargo check/test/clippy/fmt 全绿（详见决策 13）
- [x] **上游契约迁移（上次）**：schema 运行时化（getSettingsSchema），按钮 action = key，required 透传插件声明
- [x] **同步插件框架（此前）**：plugin 模块（Lua↔Transport 桥接/registry/index）、`AdapterKind::Plugin` feature 门控分派、desktop/UI 接线
- [x] **同步设置框架**：`client::settings::SyncSettings` + 引擎遵循 + UI 卡片 + 引擎重建 + 下载通知
- [x] **图片同步**：Profile 嵌 base64 data + 引擎/服务端/UI 全链
- [x] Linux 桌面修复、三形态架构 + 发布打包 + CI（详见 git 历史）

## 待办（后续功能点）

- [ ] R2~R4 桌面界面重构（五页 IA + 通道接线 + P2P 配对页，见「下一步」1~3）
- [ ] **许可证待拍板**：接入 `xime-p2p`（GPL-3.0-or-later）后本仓需整体转 GPL
- [ ] GUI 手工端到端验证 WebDAV 通道（填地址/账号 → 测试连接 → 双端同步）
- [ ] 文件同步（跨平台剪贴板文件路径读写，工作量最大）
- [ ] macOS .app 打包（LSUIElement + 图标）
- [ ] 需要 S3 等长尾云盘时，按 `Transport` trait 原生加一条通道

## 关键实现细节（本次会话）

- **P0 依赖迁移**：`xime-sync-domain` 的 git 源从 libximecore 改为 libximep2p（pin
  `b19ecdec…`）；`.cargo/config.toml` 拆两段 patch；xime-plugin pin `bdaff98`；
  client `config` → `0.15.27`；`sha2` 由 lzma-rust2 的 0.11.0 与 domain 的 0.11.1 冲突，
  需在改完 config 下限后同一次解析中一并抬到 0.11.1
- **依赖解析死锁的排法**：`cargo update -p X --precise` 会保持其它包锁定，两个包互相
  抬下限时会来回报错；把**下限写进 Cargo.toml** 再跑 `cargo metadata`，一次解析同时
  解开（`--precise` 单包更新解不开）
- **P2P 对接面已勘察**：引擎的 `Transport` trait 可直接承载 p2p（push/pull/next_event
  都有对应物），去重/回声/退避不用改；参考实现是 XimeYi 的 `device_link.rs` +
  `clipboard_sync.rs`，配对 UI 参考 `xime-setup/src/pages/pair.rs`（同为 iced 0.14）

- **收敛后的两条同步路径**：`AdapterKind::{Server,Plugin}`；默认零配置 = 本机/远端 ximed server（HTTP/WS/SSE + LocalStorage）；插件 = client 端 Lua 传输通道（云端直达），商店是唯一扩展入口；卸载激活插件自动回退本机 server
- **server 侧收缩**：`run_managed` 直用 `LocalStorage::new(data_dir)`；`StorageConfig`/`storage()` 访问器/`default_backend` 删除；default.toml `[storage]` 节删除（历史配置含 `[storage]` 的 xime.toml 不影响解析）
- **client 侧收缩**：Cargo 依赖 `hmac` 全删、`sha2/hex` 挂 `plugin` feature（商店索引 sha256 校验用）；`direct` 模块整体删除（WebDAV/S3 走插件生态，与 XimeChe 双端复用同一 .xipk）
- **schema 契约（上游 00ba2b5 起）**：schema 来自插件 Lua `getSettingsSchema()`，字段 `key/label/type/required/placeholder/helpText`；按钮 action 即按钮 key；required 透传插件声明
- **rustup 代理坑（本机开发环境）**：ZCode 沙箱改写 argv[0]，rustup 代理报 unknown proxy name；用 `~/.rustup/toolchains/stable-*/bin/{cargo,rustc}` 直连 + `RUSTC=` 环境变量绕过
- **GPG 提交**：`git commit -S`；gpg-agent 口令缓存过期后 pinentry 超时需用户重新解锁
