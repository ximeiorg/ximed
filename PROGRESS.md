# PROGRESS

> 更新：2026-11-10 · 阶段：桌面重构 R2/R3 完成；三平台托盘常驻；许可证转 GPL-3.0-or-later；HTTP 通道从界面彻底移除

## 当前状态

- **托盘常驻（本次，决策 22）**：窗口改由 `iced::daemon` 承载——关窗不再退出进程，托盘菜单
  「显示主窗口 / 隐藏到托盘 / 退出 ximed」。Windows/macOS/X11 关窗走 `Mode::Hidden`
  保留窗口；Wayland 的 `set_visible` 是 no-op，改为销毁窗口、托盘恢复时按原设置重建。
  Linux 托盘补齐 GTK/GLib 泵（`gtk_main_iteration_do`，UI 200ms 订阅）——只 `gtk::init()`
  不迭代时 AppIndicator 的 SNI 根本不注册、菜单点击无响应。侧栏 logo 的
  `image::Handle` 改为进程级缓存（每个 view 新建会换纹理 Id，泵一快就闪）。托盘图标改用
  `assets/icon.png`（macOS 22px、其余 32px），退出前 `shutdown()` 主动 drop。
  已验证：KWin 关窗 → 窗口销毁且进程存活；托盘显示 → 新窗口；托盘隐藏 → 销毁；
  托盘退出 → 进程结束（SNI 项同步消失）。
- **Release 数字 tag 触发（本次）**：`release.yml` 的 tag 过滤增加 `0.*`，`0.2.0`
  这类数字 tag 也能触发三平台安装包发布；`0.2.0` 已重新打签并 force push。
- **CI macOS runner 迁移（本次）**：GitHub 已于 2026-11-02 退役 `macos-14` 镜像；
  `ci.yml` 的测试矩阵改用 `macos-15`（arm64），`build-packages.yml` 的 macOS
  arm64/x86_64 分别改用 `macos-15` / `macos-15-intel`。
- **CI 依赖源修复（本次）**：`.cargo/config.toml` 虽然被 .gitignore 覆盖，但历史上已被 git
  跟踪；CI checkout 后它仍存在，patch 指向不存在的 `../libximecore`，导致 clippy/test 在
  解析依赖阶段直接失败。已 `git rm --cached`，`Cargo.lock` 的 5 个上游 crate 恢复 git
  source（libximecore `04d6ca9`、libximep2p `b19ecde`）。本地 patch 只作临时 override，
  跑完 `git restore Cargo.lock`。
- **自动构建调整（本次）**：`build-packages.yml` 只保留三平台桌面包（Windows x64 /
  Linux x64 / macOS arm64+x64），删除 server 与 Linux ARM/musl 目标；Linux 产物改为
  deb + AppImage + rpm，rpm 用 `cargo-generate-rpm` 生成（配置在
  `crates/app/Cargo.toml`），并新增 `deploy/ximed-desktop.desktop` 供 RPM 安装。
- **README + 截图（本次）**：新增 README（功能介绍、形态、构建/配置、架构、路线图、
  GPL 许可），并嵌入 `docs/images/overview.png` 与 `docs/images/about.png` 两张界面截图。
  按当前产品口径明确：只支持桌面形态的 P2P / WebDAV；内置 HTTP 通道与独立 server
  形态已隐藏、暂不支持。
- **许可证转 GPL-3.0-or-later（本次，决策 20 已执行）**：`LICENSE` 换成 GPLv3 全文
  （顶部加 ximed / copyright / or-later 声明）；workspace `license` 改为
  `GPL-3.0-or-later`，app/client 继承；cargo-packager 的 `license-file` 仍指向 LICENSE。
  关于页与文档同步更新。
- **关于页简化（本次，决策 23）**：入口回到侧栏普通导航（不再沉底）；按 xime-setup 的
  about 页结构收敛成三张卡：品牌 hero（logo / 版本 / 功能一句话）、应用信息（作者 /
  平台 / 仓库 / 主要依赖）、开源许可（GPL-3.0-or-later + 技术栈）。删掉 31 行依赖表，
  完整依赖清单只指向 Cargo.lock。Settings 里旧的迷你「关于」组删除。
- **选中态字体统一（上次）**：侧栏导航选中项不再切 `Weight::Semibold`——Linux 的
  Noto Sans CJK 没有 Semibold 面，fontconfig 会替换成 Bold（`fc-match` 实测），
  同一列表的选中/未选中看起来像换了字体。选中改用主色 + 实心圆点 + 浅底表达。
- **HTTP 从界面移除（上次，决策 21）**：同步页只列局域网直连 / WebDAV；设置页删掉
  「高级 · HTTP 通道」整组；概览删掉服务器启停卡；`Message::{HttpMode,HttpField}`、
  `Channel::Http` 的渲染分支、手动启停 `UiCmd::{ServerStart,ServerRestart,ServerStop}`
  全部删除。HTTP 仅作为**后台回退通道**存在（默认 `channels = ["http"]`）：界面上
  不可见、不可配，概览只在「未选择同步方式」时显示一行「内置回退 · 运行中/未就绪」。
  因为没有启停入口了，desktop 在启动时按需自动拉起本机 server（仅当 channels 含 http）
- **R3 局域网直连接线 + 配对页（上次）**：`P2pTransport`（`push`→`set_clipboard`、`pull`→
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
`cargo test --workspace` 72 项通过（client 54 + app 18）；fmt 干净。
托盘额外做了 Linux Wayland 实机验证（D-Bus StatusNotifierItem/DBusMenu + KWin 脚本
关窗，见上）；Windows/macOS 本轮只做代码路径审查，未实机跑。

## 下一步

1. **GUI 手工验证两条通道**：WebDAV（地址/账号 → 测试连接 → 双端同步）、局域网直连
   （两台设备同一网络 → 生成码/输入码 → 复制文本自动同步）；确认「未选择同步方式」
   时的提示文案。HTTP 回退/server 暂不作为对外能力（决策 24）
2. **配对端口的长期方案**：当前靠「配对前暂停节点」绕开端口冲突，代价是配对期间同步
   中断。上游给 `Watcher::bind` 加 `SO_REUSEADDR`（或配对改用临时端口 + 公告真实端口）
   后可去掉暂停逻辑
3. **恢复 HTTP / server 能力**：内置 HTTP 通道与 `ximed-server` 形态当前只保留代码，
   后续产品确认后恢复配置入口与部署文档
4. **托盘/常驻的边角**：macOS Dock 图标在无窗口时点击不会恢复窗口（winit 没有
   reopen 事件，当前只能从菜单栏托盘恢复）；GNOME 无 AppIndicator 扩展时托盘不可见，
   需要探测 StatusNotifierWatcher 并回退成「关窗即退出」；开机自启（三平台）尚未做
5. 文件同步（跨平台剪贴板文件路径读写，工程量最大）
6. macOS .app 打包（LSUIElement + 图标）

## 已完成

- [x] **Release 数字 tag 触发（本次）**：`release.yml` 支持 `v*` + `0.*`，`0.2.0` 重新 force push
- [x] **CI macOS runner 迁移（本次）**：`macos-14` → `macos-15` / `macos-15-intel`
- [x] **CI 依赖源修复（本次）**：移除误跟踪的 `.cargo/config.toml`，Cargo.lock 恢复 git source；CI 无 patch 跑通 clippy/test
- [x] **自动构建收敛（本次）**：三平台 desktop-only；Linux deb/rpm/AppImage；删除 server/ARM 目标
- [x] **README + 界面截图（本次）**：概览/关于页截图入库，README 覆盖功能、构建、配置、架构、许可与路线图
- [x] **许可证转 GPL-3.0-or-later（本次，决策 20）**：LICENSE 全文 + workspace license + About 页 + 文档一致
- [x] **关于页简化（本次，决策 23）**：侧栏普通入口 + 三张卡（hero / 应用信息 / 开源许可），依赖表收敛为 Cargo.lock 指向
- [x] **三平台托盘常驻（上次，决策 22）**：`iced::daemon` + 关窗隐藏/销毁重建 + Linux GTK 泵 + 官方图标；手工验证 Linux Wayland 关窗/显示/隐藏/退出全链路
- [x] **R2/R3 桌面重构（上次）**：五页 IA + 通道模型/配置迁移 + `Router` 降级 + P2P 配对页；`UiState` 改单一草稿；视觉对齐 xime-setup 组件（详见决策 17~19）
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
