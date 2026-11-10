# PROGRESS

> 更新：2026-11-10 · 阶段：局域网 P2P 接入（P0 依赖迁移完成；P1 设备层待做）

## 当前状态

- **P0 依赖迁移（本次）**：domain 从 libximecore 改到 libximep2p（pin `b19ecde`，与
  libximecore / XimeYi 同源，避免两个 `Profile` 类型）；`.cargo/config.toml` 拆成两段
  patch；xime-plugin 临时 pin `bdaff98`（mlua 时代最后 rev，上游已迁 quickjs）；
  client `config` 下限抬到 0.15.27。仓库恢复可构建：`cargo check --workspace`（默认
  与 `--no-default-features`）双绿、`cargo test --workspace` 50 项全过、fmt 干净
- **同步模型收敛（上次）**：按决策 13，删除三套 WebDAV 实现中的两套——server 端 `xime-sync-store` WebDAV 存储后端（webdav feature/WebDavStorage/build_storage 工厂，server 直用 LocalStorage，`[storage]` 配置节删除）、client 直连 WebDAV/S3 适配器（`AdapterKind::{WebDav,S3}`/`direct` 模块/SigV4/4 个 TCP mock 端到端测试）；`hmac` 依赖删除，`sha2/hex` 改为 plugin feature 可选（仅商店校验）。保留两条路径：默认 server（本机/远端 hub）与插件（云端直达，商店承载）。两个仓库均已全绿
- **上游契约迁移（更早，已提交 d9a46fa）**：schema 由插件 Lua `getSettingsSchema()` 运行时返回（上游删除 `PluginManifest.config_schema` 与 `has_fn`），ximed 侧 `config_fields`/`test_connection` 适配完成
- **同步插件框架**：复用 libximecore `xime-plugin`（.xipk 安装/Lua 沙箱/clipboard_sync 契约），ximed 侧补齐 schema 解释器、SyncPluginManager、插件商店索引
- **同步独立菜单**：侧栏「同步」页 = 同步方式（插件单选激活 + schema 动态表单 + 测试连接 + 恢复默认）+ 同步开关卡片 + 插件商店
- **ximed_sync 插件自动填充**：desktop 后台把本机 server 连接信息经 `UiMsg::LocalServerInfo` 下发；空字段自动填入（已保存值不覆盖）

## 下一步

1. **P1 设备层**：`client/src/p2p.rs`（feature `p2p`）——`device_link` 进程级单例
   （`OnceLock<Mutex<Option<Node>>>`，照 XimeYi `device_link.rs`）+ 数据目录
   `{data_dir}/p2p/{identity.bin,peers.json}`。注意 `Node::start`/`wait_event` 会阻塞，
   必须走 `spawn_blocking` 或独立线程，且单例永不 drop（`Drop` 会 `shutdown_timeout` 阻塞）
2. **P2 传输适配**：`AdapterKind::P2p` + `P2pTransport`（push→`set_clipboard`、
   pull→`Ok(None)`、next_event→`wait_event`）+ 同步页「局域网直连」选项；用
   `Event::Peer{connected}` 维护连接态，无对端时 push 返错复用引擎退避
3. **P3 配对 UI**：`Panel::Device` + 侧栏「设备」页（显示码/输入码/已配对设备表），
   线程模型照 `xime-setup/src/pages/pair.rs`（会话号递增丢弃迟到事件）。**已知坑**：
   `Watcher::bind` 无 `SO_REUSEADDR`，Node 常驻时 `join()` 绑不上 UDP 47822 → 配对前
   暂停节点或只走 `join_at` 手动地址
4. 上游登记两项：xime-plugin 迁移 quickjs 契约（解开 `bdaff98` pin）、`Watcher` 加
   `SO_REUSEADDR`
5. GUI 手工验证：插件同步端到端（见「待办」）

## 已完成

- [x] **P0 局域网 P2P 依赖迁移（本次）**：domain 迁 libximep2p（pin `b19ecde`）+ patch 拆段 + xime-plugin pin `bdaff98` + config 下限 0.15.27；两种构建形态 check 双绿、50 项测试全过、fmt 干净（详见决策 14）
- [x] **同步模型收敛（上次）**：server 只保留 local 后端与默认 hub/官方协议定位；client 只保留 Server/Plugin 两种适配器；删除 server 端 WebDAV 存储后端、client 直连 WebDAV/S3（含 SigV4）与 store_factory 装配层；`[storage]` 配置节与相关环境变量移除；libximecore 仓库级 reqwest 依赖删除；两仓库 cargo check/test/clippy/fmt 全绿（详见决策 13）
- [x] **上游契约迁移（上次）**：schema 运行时化（getSettingsSchema），按钮 action = key，required 透传插件声明
- [x] **同步插件框架（此前）**：plugin 模块（Lua↔Transport 桥接/registry/index）、`AdapterKind::Plugin` feature 门控分派、desktop/UI 接线
- [x] **同步设置框架**：`client::settings::SyncSettings` + 引擎遵循 + UI 卡片 + 引擎重建 + 下载通知
- [x] **图片同步**：Profile 嵌 base64 data + 引擎/服务端/UI 全链
- [x] Linux 桌面修复、三形态架构 + 发布打包 + CI（详见 git 历史）

## 待办（后续功能点）

- [ ] P1~P3 局域网 P2P 接入（见「下一步」1~3）
- [ ] 上游 xime-plugin 迁移 quickjs 契约后放开 `bdaff98` pin
- [ ] **许可证待拍板**：接入 `xime-p2p`（GPL-3.0-or-later）后本仓需整体转 GPL
- [ ] GUI 手工端到端验证插件同步
- [ ] 文件同步（跨平台剪贴板文件路径读写，工作量最大）
- [ ] macOS .app 打包（LSUIElement + 图标）
- [ ] 按需评估插件商店补 S3 同步插件

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
