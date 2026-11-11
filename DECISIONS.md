# DECISIONS

> 重要决策记录。原设计文档 docs/design/clipboard-sync.md 已删除（其中 webdav/s3 为
> server 存储后端的方案已被废弃——实际实现为 client 直连，见下），决策在此独立维护。

## 1. 所有字段一律小写

JSON 字段、Rust 结构体字段、数据库列名统一 snake_case 小写（`has_data`/`data_name`），不使用大写开头、不使用 camelCase、不使用 PascalCase；枚举值序列化为小写（`"type": "text"`）。放弃与 SyncClipboard 字面协议兼容（其字段为 PascalCase），保留 Profile/Hash 算法概念；若未来需对接 SyncClipboard 桌面客户端，在协议适配层做大小写转换。

## 2. 三通道并存（HTTP 主 / WS / SSE 可选）

Xime 手机端只接入 HTTP 事件驱动同步（零长连接、最省电、符合 Android 10+ 后台剪贴板访问限制）；WS/SSE 供未来桌面端/浏览器等其他实时客户端选用，不强制降级链。

## 3. 存储抽象为 Storage trait（现仅 Local，收敛见 13）

本地文件（默认）唯一后端；Storage trait 保留（业务代码不感知、测试可 mock）。
WebDAV/S3 存诸后端已随决策 13 移除（server 不再背负存储后端）。

## 4. 当前剪贴板单条数据用文件存储而非数据库

DB 是过度设计；`TursoStorage`（libsql）留到历史记录（P7）。

## 5. Basic Auth 而非 token

无状态、低开销、三通道共用；未来可平滑升级。

## 6. HTTPS 用 rustls

硬性规则禁止 openssl；或前置反向代理。

## 7. ~~S3/WebDAV 为可选 feature~~（已废弃，见 13）

原方案在 server 引入 WebDAV/S3 存储后端 feature；现全部走同步插件（决策 13），
client 直连适配器与 server 存储后端一并移除，依赖体积进一步下降。

## 8. 不引入 MQTT

Xime 手机端已采用 HTTP 事件触发（零长连接，最省电），MQTT 的省电优势建立在常驻连接之上，反而更费；未来若出现真实"实时推送"客户端需求，再评估。

## 9. 历史记录用 libsql 手写 SQL，不引入 ORM

toasty 0.9.0 是唯一原生 Turso driver ORM 但 API 不稳定；本地嵌入式 Turso 是 SQLite 兼容；历史查询复杂度低，手写 SQL 足够。若未来需对接 Turso 云库再评估 toasty。

## 10. 配置用 config-rs，采用"内嵌默认值 + 环境变量 + 多路径文件"范式

`MemorySource::new()`（`include_str!` 内嵌 `default.toml`）→ `Environment::default().prefix("XIMED").separator("__").try_parsing(true)`（双下划线嵌套映射，`XIMED__SERVER__ADDR` → `server.addr`）→ `./ximed.toml` → `/app/ximed.toml` → `/etc/ximed/config.toml`（后添加者优先）；`Settings` 结构 + 访问器方法；敏感项（认证密码、存储凭据、TLS 证书）一律环境变量注入不落盘明文，认证密码经 `auth.password_env` 指定环境变量名（默认 `XIMED_AUTH_PASSWORD`）启动时 `std::env::var` 读取；实测 config 0.15 不引入 openssl/native-tls。

## 11. Workspace 按层拆分独立 crate，依赖单向

crates 目录去 `ximed-` 前缀，四层单向依赖 server → service → store → domain：
- `domain`：领域模型（Profile/hash/protocol）+ Storage trait 抽象，零业务依赖
- `store`：存储实现（LocalStorage）+ 仓库（ClipboardRepo）+ 后端装配
- `service`：业务逻辑（ClipboardService：校验/幂等/广播/缓存）+ ClipboardContext
- `server`：handler 层（axum HTTP/WS）、auth、config、AppState 装配、main
避免在单个大 crate 中混层，每层可独立测试与复用；domain 命名避开 Rust 内建 `core` 命名冲突。

## 12. 同步插件复用 libximecore xime-plugin，插件配置以 config/\<id\>.yaml 为唯一真源

核心（.xipk 安装、Lua 沙箱运行时、clipboard_sync 契约）由 libximecore `xime-plugin` 承载，XimeChe 与 ximed 双端复用；ximed 侧只补"宿主消费"层（configSchema 解释、管理器、商店索引）。决策点：
- **插件配置唯一真源是 `plugins/config/<id>.yaml`**（PluginManager 约定路径，与插件运行时 `host.config` 同源）：同步页表单只是该文件的编辑器，AppSettings 仅存激活的 `plugin_id`，不存配置值——插件自身经 host.config 写的值与表单天然一致
- **同步配置 UI 只走插件，不暴露原生适配器表单**：侧栏「同步」页 = 插件单选激活（选中即激活）+ configSchema 动态表单 + 测试连接 + 同步开关 + 商店；原生 server/WebDAV/S3 表单从桌面 UI 移除（保留为 daemon/ximed.toml 配置能力与默认零配置路径），仅留「恢复默认（连接本机 server）」一键回退；卸载激活插件自动回退本机 server
- **插件实例目录独立**：`{config_dir}/plugins`，不与 XimeChe 的 `~/.config/xime/plugins` 共享（其 daemon 扫描重载 emoji 插件，registry 混用互相干扰）；.xipk 包双端通用
- **configSchema 解释约定**（此前 libximecore 无任何解释器）：字段 `key/label/type/required/placeholder/helpText/action`，type ∈ text|secret|button（未知按 text 宽松兜底），值统一字符串化；先落 ximed，XimeChe 需要同款表单时再上移 xime-plugin crate
- **UI 与插件层解耦**：ui.rs 只消费纯数据结构（InstalledPlugin/PluginFieldSpec/StorePlugin），client::plugin 类型转换收敛在 desktop.rs，UI 不感知 feature 门控

## 13. 同步模型收敛：只保留「默认本机/远端 server + 同步插件」两条路径（2026-09-16）

插件即 client 端传输通道（WebDAV/S3/其他云端 .xipk 与 XimeChe 双端复用），激活后数据
在设备 ↔ 云端间直达，本机 server 不参与同步——server 只保留两个定位：默认零配置本地
hub（手机 Xime IME 走 HTTP 事件驱动连它）与官方远端协议端点（headless daemon /
`ximed_sync` 插件）。据此移除三套实现中的两套：
- **server 端 WebDAV 存储后端删除**：`xime-sync-store` 的 `webdav` feature、`WebDavStorage`、
  `build_storage`/`BackendKind` 工厂整体移除，server 直用 `LocalStorage`；`[storage]` 配置节
  一并删除（此前该 feature 从未被任何构建启用，`backend="webdav"` 原静默回退 local）
- **client 直连 WebDAV/S3 适配器删除**：`AdapterKind::{WebDav,S3}`、`direct` 模块（含手写
  SigV4～S3Transport）、相关配置字段/环境变量与 4 个 TCP mock 端到端测试全部移除；
  `hmac` 依赖随之删除，`sha2/hex` 改为 `plugin` feature 可选（仅商店 sha256 校验用）
- **保留两条路径**：`AdapterKind::{Server,Plugin}`；要换远端存储/绕过 hub 一律从插件商店
  装 clipboard_sync 插件；`required` 语义等 schema 契约同决策 12（更新：schema 已上移为
  Lua `getSettingsSchema` 运行时契约，见上游 00ba2b5）

## 14. 同步依赖迁到 libximep2p；xime-plugin 暂钉 mlua 时代 rev（2026-11-10）

**背景**：本仓上次提交（64063e0，09-16）之后，上游把 `xime-sync-domain` 从 libximecore
迁到独立仓 libximep2p（GPL-3.0-or-later），ximed 仍从 libximecore git 取该 crate，
`.cargo/config.toml` 的 patch 指向已不存在的路径——`cargo metadata` 在依赖解析阶段
直接失败（exit 101），整个仓库不可构建。

决策点：

- **domain 的来源改到 libximep2p，并 pin 与 libximecore / XimeYi 相同的 rev**
  （`b19ecde…`）。两者若指向不同 rev，会解析出两个 `xime-sync-domain`，`Profile`
  成为两个不同类型，跨 crate 传参（后续 `node.set_clipboard(profile)`）直接编译失败。
  这也是后续接入 `xime-p2p` 的前提。
- **`.cargo/config.toml` 拆成两段 patch**（libximecore / libximep2p），本地开发分别指向
  `../libximecore` 与 `../libximep2p`。patch 必须打在 libximep2p 这个源上，否则 git 依赖
  仍会去拉远端。
- **xime-plugin pin 在 `bdaff98`**（上游 quickjs 迁移 `8e25e50` 的父提交）：该 rev 已含
  `get_settings_schema` 契约，运行时仍是 mlua，与 client 现有插件适配层匹配。上游
  `8e25e50` 起把运行时换成 quickjs-rusty（插件脚本 Lua → JS），直接跟 HEAD 有 49 个
  编译错误（`PluginRuntime` 不再 `Send + Sync`、`SettingField` 等 API 变更）。迁移 JS
  契约是独立功能点，本次不做。**该 pin 是临时状态**，迁移完成后放开。
- **client 的 `config` 下限抬到 0.15.27**：`xime-sync-server` 的 workspace 声明是
  `^0.15.27`，两侧下限不一致时同一依赖图无法统一到同一版本（`cargo update` 单包
  更新会来回互相卡死）。
- **许可证**：`xime-sync-domain` 现为 GPL-3.0-or-later（原来在 libximecore 里是
  MIT），libximep2p 的 `xime-p2p` 同为 GPL。ximed 当时仍是 MIT——接入 p2p 后整体分发
  必须转 GPL-3.0-or-later；这条已在决策 20 执行完毕。

## 15. 局域网直连：节点归设备层单例，feature 默认关（2026-11-10）

P2P 作为第三种同步通道接入（协议文档 §2.2 的定位），与 server / 插件互斥选用。分层：

- **配对是认证，同步是功能**（照 XimeYi `device_link.rs`）：`Node`（监听 / 发现 /
  会话）属于设备层，剪贴板同步只是它的订阅者。以后加词库、主题同步时节点不重建。
- **节点必须是进程级单例**（`OnceLock<Mutex<Option<Node>>>`，`client/src/p2p.rs`）：
  desktop 每次「保存并应用」都会 abort 引擎任务再重建 `SyncEngine`
  （`stop_client_services` / `spawn_client_services`）。节点若跟着引擎走，改一次设置
  就重建一次节点——重新抢端口、丢掉已建连接，表现为「点了保存，同步断一会」。
- **线程约束写进模块文档**：`Node::start` 与 `wait_event` 内部 `block_on`，异步上下文
  直接调用会 panic，必须 `spawn_blocking` 或独立线程；单例在进程存活期间不 drop
  （`Drop` 会 `shutdown_timeout` 阻塞）。
- **数据目录固定 `<base>/p2p`**（`identity.bin` + `peers.json`）：配对入口与常驻节点
  必须同一目录，否则是「设置里配对成功、同步仍认为未配对」且界面看不出异常。
- **配对表读失败必须报错，不回落空表**：表里是用户已建立的信任关系，静默清空等于把
  所有设备退回未配对，而界面只会显示「暂无已配对设备」。
- **feature `p2p` 默认关**：server 形态（树莓派等低性能设备）不引入 xime-p2p 的密码学
  依赖（snow / ed25519 / x25519 / chacha20poly1305 / spake2）与网络栈；桌面形态在
  P2 接线时再并入 `tray`。

## 16. 移除插件体系（2026-11-10，作废决策 12/13 中插件相关部分）

插件（.xipk + 运行时 + 商店）整体删除，同步只保留原生通道。理由：

- **主要用途已被原生通道覆盖**：插件最初是 WebDAV / S3 的唯一载体；WebDAV 转为原生、
  局域网直连原生实现后，插件只剩长尾网盘价值，而长尾需求用一条原生通道（今天加
  WebDAV 只花了约 150 行 + 5 个测试）比背一套运行时便宜得多。
- **成本不对称**：插件运行时是一个 C 引擎（原 mlua，上游已换 quickjs-rusty）+ 沙箱
  + 契约版本管理 + 商店索引。与「极低性能设备也能部署」的目标直接冲突。
- **已实际付过一次代价**：上游把运行时从 mlua 换成 quickjs，让本仓从「能跑」变成
  49 个编译错误，只能把 `xime-plugin` 钉在迁移前的 `bdaff98`。删除后这条 pin 债与
  `xime-plugin` 依赖一并消失（决策 14 的临时状态解除）。
- **生态归属不在本仓**：`.xipk` 跨端复用的价值属于 libximecore / XimeChe / XimeYi 侧。

删除范围：`client/src/plugin/`（1043 行含测试）、`plugin` feature 与
xime-plugin/serde_yaml/sha2/hex/zip 依赖、`AdapterKind`（原 Server/Plugin 二选一）、
`AppSettings::{adapter, plugin_id}`、桌面 UI 的插件列表/动态表单/商店，
以及 `UiMsg`/`UiCmd` 的 8 个插件消息。

兼容性：

- 旧配置里的 `adapter` / `plugin_id` / `plugins_dir` 字段**被 serde 忽略**（未声明为
  `deny_unknown_fields`），解析不会失败，回落到默认的 HTTP 通道。
- 插件通道用户的 `plugins/config/<id>.yaml` 与 `.xipk` 无法自动转成原生通道参数，
  需在同步页重新填一次；`plugins/` 目录可留可删，代码不再读取。
- 想写自定义传输后端时再评估扩展机制，届时用 wasm（wasmi 之类）而不是 JS/Lua 沙箱。


## 17. 通道模型：三个内置通道 + 有序列表 + 只让 P2P 参与降级（2026-11-10）

同步通道收敛为 `http` / `webdav` / `p2p` 三个内置 id 之后：

- **有序列表，同刻只跑一条**：剪贴板是单值资源，双写会重复推送、回写还会互相触发。
  列表顺序即优先级，`channel::decide` 决定当前用哪一条；配置字段是
  `channels = ["p2p", "webdav"]`，与 XimeYi / Android 同语义。
- **只有 P2P 参与健康判定**：P2P 有 `Event::Peer` 的真实连接态；HTTP / WebDAV 只有
  「这次 push 成不成」，一次失败可能只是抖动，拿它切换会横跳。其余通道按用户给的
  顺序一直用，直到用户改配置。
- **未知 id 直接丢弃并告警**（作废决策 15 时期的「宽松解析成插件」）：宽松接受会让
  「写错 id」表现为同步静默关闭，比直接报错更难排查。
- **列表为空 / 全部不可用 → 退回 HTTP**：旧用户升级后同步不能因为配置为空而停摆。
- **切换对引擎透明**：`client/src/router.rs` 的 `Router` 实现 `Transport`，每轮调用前
  按 `decide` 复核、必要时换一条再转发。引擎的去重 / 回声抑制 / 退避状态不因切换重置。

## 18. 桌面配置改为「每通道一节 + 单一草稿」（2026-11-10）

`AppSettings` 从扁平的 `mode` / `remote_*` 改为：

```toml
channels = ["p2p", "webdav"]
[http]    # mode / url / username / password
[webdav]  # url / file / username / password(_env)
[p2p]     # device_name
[sync]    # 开关与白名单
```

- **迁移在 `load()` 里做一次**：旧扁平字段用 `#[serde(flatten, skip_serializing)]`
  读入，`migrated()` 搬进 `[http]` 后清空；保存时写新形状，旧 key 不再出现。
- **`p2p.data_dir` 不落盘**：它由宿主按部署位置注入（`<config_dir>/p2p`），属于环境
  而非用户偏好；写进配置文件会让「换目录后配对表找错」。
- **UI 只持有一份 `AppSettings` 草稿**：原来 20 多个平行字段里「改了一半的界面」
  与「已保存的配置」混在一起，保存失败后界面显示的是没生效的值。

## 19. 桌面视觉对齐 xime-setup 组件词汇（2026-11-10）

不再自造样式，直接对齐 `libximecore/crates/xime-setup`：

- 主题常量取 `ThemeColors` 深色一档（背景 `#09090b`、卡片 `#141417`、轨道 `#26262b`、
  控件底 `#212126`），圆角 14/12/8，控件统一高 36。
- 组件：`settings_group`（标题 + 描述 + 条目）、`settings_item`（左 label/描述 + 右控件）、
  `tab_bar`（等宽药丸分段）、`badge`、`banner`（成功/失败提示条）、主题化 `toggler`、
  细进度条；侧栏改「分组标题 + 导航项」。
- 布局语义照 `pages/pair.rs`：配对方向用分段控件分开，一次只显示一个主按钮；码用
  34px 主色 + 倒计时进度条；另一方向还在跑时给出跨 tab 提示。
- **选中态不切字重**：侧栏导航 active 原先切 `Weight::Semibold`，但 Linux 的
  Noto Sans CJK 没有 Semibold 面，fontconfig 会替换成 Bold（`fc-match` 实测），
  同一列表的选中/未选中看起来像两种字体。选中只用主色 + 实心圆点 + 浅底，
  字重保持与未选中一致。

## 20. LICENSE：已切换为 GPL-3.0-or-later（2026-11-10 执行）

背景：`xime-p2p` 与 `xime-sync-domain` 都是 GPL-3.0-or-later；`tray` feature 默认
依赖 `client/p2p`，默认桌面产物早已链入 GPL 代码，继续以 MIT 分发不成立。

决定：**整体转 GPL-3.0-or-later**（与 XimeYi / Android 一致），不采用「把设备页
feature 化」的规避方案——局域网直连是桌面版的产品核心。

- `LICENSE` 换成 GPLv3 全文，顶部保留 ximed / Copyright (C) 2026 kingzcheung /
  “either version 3 ... or any later version” 声明；GPL 文本本身不改。
- workspace `Cargo.toml` 的 `license = "GPL-3.0-or-later"`；app / client 的
  `license.workspace = true` 自动继承；cargo-packager 的 `license-file` 继续指向
  `LICENSE`。
- 关于页的许可证卡改为 GPL-3.0-or-later（`APP_LICENSE` 取自 `CARGO_PKG_LICENSE`，
  测试 `app_license_is_gpl` 防止回退）。
- 兼容性：现有依赖是 MIT / Apache-2.0 / Zlib 等宽松许可，与 GPL 兼容；xime-p2p /
  xime-sync-domain 同为 GPL。再分发二进制时需同时提供对应源码。

## 21. HTTP 通道从界面彻底移除，只留后台回退（2026-11-10）

需求：现阶段不提供 HTTP 同步的配置能力。上一版只是把它「降级藏起来」，结果同步页有
一条「HTTP 服务 · 后台默认通道」、设置页有「高级 · HTTP 通道」（模式/地址/账号/密码），
与需求不符。

处理：

- **同步通道页只渲染局域网直连与 WebDAV**：`UiState::visible_channels()` 过滤掉
  `http`；空列表时显示「还没有选择同步方式」，添加按钮也只有那两个。
- **设置页删掉整组 HTTP 配置**；概览删掉服务器启停卡（含地址、端口、重启按钮），
  `server_url` 字段一并删除。
- **HTTP 仍是默认回退**（首次启动 `channels = ["http"]`）：老用户升级后同步不中断。
  它在界面上只以一行「内置回退 · 运行中/未就绪」出现，且仅在用户尚未选择同步方式时。
- **本机 server 改为按需自动启动**：启停入口没了，若还要求手动开启，默认回退就是死的。
  启动条件限定为「`channels` 含 http」——用户改用局域网/WebDAV 后不再监听端口；
  已经启动的 server 不会因为后来切走而停掉（它同时是其它设备的 hub）。
- 删除手动启停的 `UiCmd::{ServerStart,ServerRestart,ServerStop}` 与对应后台分支；
  server 由 `ServerHandle { task, _shutdown }` 持有（sender 与任务同寿命，否则
  `run_managed` 会立刻退出）。

以后要恢复 HTTP 配置能力时，只需把这几个渲染分支加回来并接上 `draft.http` 字段——
配置结构 `[http]` 一直保留，迁移与引擎侧都不用动。

## 22. 托盘常驻：iced daemon + 平台差异化隐藏（2026-11-10）

产品语义：窗口只是入口，进程常驻；点窗口关闭按钮不退出，托盘菜单提供
「显示主窗口 / 隐藏到托盘 / 退出 ximed」，只有「退出 ximed」结束进程。
三平台（Windows / Linux / macOS）同一套菜单与语义。

实现决策：

- **用 `iced::daemon` 承载桌面 UI**：`iced::application` 在最后一个窗口销毁时会退出进程，
  而 daemon 明确「所有窗口关闭后也不停止」，这是托盘常驻的基础。启动时由 boot 返回
  `window::open(settings)` 开主窗口；「显示主窗口」时若窗口已被销毁则按同一份
  `window::Settings` 重建。窗口图标、Linux `application_id` 都放进这份 settings，
  重建时不会丢。
- **关窗策略按平台分**：Windows / macOS / X11 用 `window::set_mode(Hidden)` 保留窗口，
  尺寸与界面状态都不丢；Wayland 上 winit 的 `Window::set_visible` 是空实现
  （`winit-0.30.13/src/platform_impl/linux/wayland/window/mod.rs:253`），窗口藏不掉，
  只能 `window::close` 销毁、显示时重建（`should_recreate_window()` 按
  `WAYLAND_DISPLAY` / `WAYLAND_SOCKET` 判断）。
- **Linux 托盘必须泵 GLib 主上下文**：tray-icon 的 GTK 后端是
  libayatana-appindicator，SNI 注册与 DBusMenu 激活都挂在 GLib 默认上下文上；只
  `gtk::init()` 而 iced/winit 不跑 GTK 主循环时，托盘项**不会注册**、菜单点了也没反应
  （已用 D-Bus 实测）。因此在 iced 主线程初始化 GTK，并在 UI 订阅里加一条 200ms 定时器
  调 `gtk::main_iteration_do(false)`；所有 GTK 调用保持主线程。Windows/macOS 由
  tray-icon 自己的消息循环驱动，pump 为空实现。
- **托盘图标用官方 `assets/icon.png`**：macOS 22px、其余 32px，按平台 Lanczos 缩放；
  不再程序化画蓝点。macOS 不开 template——源图有不透明圆角底，template 会变成实心块。
- **侧栏 logo 的 `image::Handle` 必须缓存**：`Handle::from_bytes` 每次生成新 `Id`，
  渲染器会重新上传纹理；泵把 UI 重绘频率抬高后，表现为 logo 持续闪烁。用
  `OnceLock` 只建一次，`view` 里 clone 同一个句柄。泵间隔取 200ms（5Hz），
  在菜单响应与空转开销之间折中。
- **托盘句柄存主线程 thread-local**：既保证托盘活到进程结束，又在退出前 `shutdown()`
  主动 drop，让 Windows 立即删图标、Linux 删临时 PNG（`Box::leak` 做不到）。
- **托盘创建失败则关窗即退出**：不能把窗口藏起来让用户既看不到界面也无法退出。
  创建时机从「首个后台事件」提前到「窗口打开 / 首个 Linux 泵 tick」，避免引擎异常时
  永远没有托盘。
- macOS 菜单栏点击不在 `TrayIconEvent::Click` 里顺手弹窗口（点击本身会展开菜单）；
  Windows/Linux 保留左键点击显示窗口；右键只出菜单（只匹配左键 Up）。

已知边角（后续）：macOS 无窗口时点 Dock 图标不会恢复（winit 没有 reopen 事件）；
GNOME 无 AppIndicator 扩展时托盘不可见，需探测 StatusNotifierWatcher 后回退成关窗即退出；
开机自启尚未做。


## 23. 关于页：对齐 xime-setup 的简洁信息页（2026-11-10）

`Panel::About` 入口放在侧栏普通导航里（不再用 Fill 垫到底部），desktop / client-only
都可访问；Settings 里旧迷你「关于」组删除。

- **结构参考 `libximecore/crates/xime-setup/src/pages/about.rs`**：三张卡即止——
  1. 品牌 hero：logo、名称、版本胶囊、一句话功能（局域网直连 / WebDAV / 剪贴板 /
     托盘常驻）；
  2. 应用信息：作者、运行平台、仓库、主要依赖 + 「完整依赖清单见 Cargo.lock」；
  3. 开源许可：GPL-3.0-or-later 胶囊、版权、许可证指引、技术栈。
- **不再内嵌依赖表**：之前列了 31 行 app/client 直接依赖，信息密度过高、且静态
  版本表容易过期；现在只给主要依赖与 Cargo.lock 指针。功能也从 6 条列表收敛成
  hero 里的一句话。
- **许可证与决策 20 一致**：显示 GPL-3.0-or-later（`APP_LICENSE` 取自
  `CARGO_PKG_LICENSE`），不再出现 MIT/GPL 并存的提示；`app_license_is_gpl`
  测试防止许可证回退。


## 24. HTTP 通道与独立 server 形态暂不对外支持（2026-11-10）

当前对外支持范围收敛为桌面形态的两种原生通道：局域网直连（P2P）与 WebDAV。
内置 HTTP 通道已从界面隐藏、不提供配置；`ximed-server` 独立部署形态暂不支持。
README、关于页和路线图按这个口径描述，不再把 HTTP 回退或 server 部署写成现有能力。

代码层面决策 21 的后台 HTTP 回退、本地 server 自动拉起与 `ximed-server` 二进制仍保留，
用于旧配置兜底与后续恢复；它们不再作为对外承诺。若产品决定彻底禁用，需要改默认
`channels`、停止 server 自动拉起，并去掉概览里的「内置回退」行——那是独立的行为改动，
不在本次文档修正范围内。恢复时删除本决策并同步 README / 关于页。


## 25. 自动构建收敛为三平台桌面包，Linux 增加 rpm（2026-11-10）

按决策 24 的产品口径，`build-packages.yml` 不再构建 server / client，只出三平台桌面安装包：

- **矩阵**：Windows x86_64、Linux x86_64、macOS arm64 + x86_64（macOS 两个架构）；
  删除 Linux aarch64/armv7 musl server 目标。
- **Linux 产物**：`.deb`、`.AppImage` 由 cargo-packager 生成；`.rpm` 由
  `cargo-generate-rpm` 生成，配置在 `crates/app/Cargo.toml` 的
  `[package.metadata.generate-rpm]`（binary、desktop entry、图标、LICENSE）。
- **为什么不用 nfpm / cargo-bundle / cargo-rpm**：
  - `cargo-bundle` 0.12 的 rpm 实现是 `unimplemented!()`，不能生成 rpm；
  - `cargo-rpm` 0.8 较旧且依赖 `rpmbuild`；
  - `cargo-generate-rpm` 是维护中的纯 Rust 实现，不需要 rpmbuild，和 Cargo metadata
    同源；
  - cargo-deb 能生成 deb，但 cargo-packager 已经同时覆盖 deb/AppImage，再引入
    cargo-deb 会增加一套重复配置；后续如果要把 deb 也改成纯 Cargo 工具，可再评估。
- **桌面入口**：新增 `deploy/ximed-desktop.desktop`，RPM 与后续其它 Linux 包装共用；
  AppImage/deb 仍由 cargo-packager 自动生成 desktop/icon。
- 构建仍使用 `actions-rust-cross` 产出的 `target/<target>/release/ximed-desktop`，
  `cargo-generate-rpm` 只打包不重编。


## 26. 依赖源：CI 用 git + Cargo.lock，本地 patch 不入库（2026-11-10）

CI 在依赖解析阶段报：

```text
patch location `/home/runner/work/ximed/libximecore/crates/xime-sync-server`
does not contain packages matching `xime-sync-server`
```

原因：`.cargo/config.toml` 虽然写在 `.gitignore` 里，但历史上已经被 git 跟踪（ignore 对
已跟踪文件无效）。CI checkout 后该文件存在，`[patch]` 指向 `../libximecore` /
`../libximep2p`，而 runner 上没有这两个目录，`--locked` 直接失败。

处理：

- `git rm --cached .cargo/config.toml`：文件从索引移除，本地文件保留、继续被 ignore。
- `Cargo.lock` 恢复 5 个上游 crate 的 git source：
  - `xime-p2p` / `xime-sync-domain`：libximep2p rev `b19ecde`
  - `xime-sync-server` / `xime-sync-service` / `xime-sync-store`：libximecore
    commit `04d6ca9`（Cargo.lock 锁定，CI `--locked` 可复现）
- CI / 发布构建始终使用 git 依赖；本地 patch 只作为开发时临时 override。带 patch 跑
  `cargo check` 会把 lock 改成 path source，提交前必须 `git restore Cargo.lock`；带
  patch 时不要加 `--locked`（会报 lock 需要更新）。
- 已在无 patch 环境下原样跑通 CI 的 clippy（默认 + 无默认特性）与
  `cargo test --workspace --all-targets --locked`。

## 27. Windows GUI 入口必须声明 windows_subsystem（2026-11-11）

用户反馈 Windows 启动时弹一个 cmd 黑窗。**与 server 无关**：desktop 形态里 server 是
同进程 `tokio::spawn`，不创建子进程也不创建控制台。真正原因是 Rust 二进制默认按
**console 子系统**链接，双击时系统先为它建一个控制台窗口，并挂到进程退出。

- `ximed-desktop`：`#![cfg_attr(windows, windows_subsystem = "windows")]`。
  `required-features = ["tray"]` 已保证它是 GUI 形态，无需再判断。
- `ximed-client`：`#![cfg_attr(all(windows, feature = "tray"), windows_subsystem = "windows")]`。
  带 tray 是 GUI 不该弹窗；`--no-default-features` 的 daemon 形态必须保留控制台，
  否则日志与 `--help` 全都没了。
- `ximed-server`：保持 console，服务端输出就是它的观测面。
- 资源脚本 `app.rc` 只嵌图标，没有 `/SUBSYSTEM` 声明，所以属性在 crate 根部即可生效。

**连带问题**：GUI 子系统下进程没有 stdout，`tracing_subscriber` 的默认 writer 会把日志
写进 void，等于修好黑窗却彻底失去排障能力。因此新增 `app::logging`：

- 写 `<配置目录>/logs/<bin>.log`，`with_ansi(false)`；
- 启动时若超过 5 MiB 滚动成 `.1`，只留一份历史，够定位最近一次运行；
- 只有 Windows 调用它，但模块本身三平台编译——本地没有 Windows 工具链时也能验证类型，
  CI 也会持续覆盖；
- macOS / Linux 仍走 stdout，`cargo run` 开发体验不变。

## 28. 依赖源补充：libximecore 钉 rev，不跟随默认分支（2026-11-11）

决策 26 已定下「CI 用 git 依赖、本地 patch 不入库」，`Cargo.lock` 记录的是不钉 rev 时
解析到的 `04d6ca9`。本次在该基础上把三个同步服务 crate 钉到显式 rev：

```toml
xime-sync-store   = { git = ".../libximecore.git", rev = "b4714f6", package = "xime-sync-store" }
xime-sync-service = { ... 同上 }
xime-sync-server  = { ... 同上 }
```

- **为什么钉**：不钉 rev 时，本地 patch 解析到的是邻居仓库 HEAD、CI 解析到远端默认分支
  最新——同一份 lock 在两边可能指向不同代码，本地全绿不代表 CI 能过。
- **选哪个 rev**：`b4714f6` 的 `Cargo.toml` 内部引用 libximep2p `b19ecde`，与本仓
  domain / p2p 的 pin 同代，不会解析出两个 `Profile` 类型。
- **lock 必须同步重生成**：改 rev 后旧 lock 的 source 不匹配，`--locked` 会报需要更新；
  在无 patch 状态跑一次 `cargo metadata` 即可。
- **本地 patch 的代价照旧**：带 patch 跑 cargo 会把 lock 的 source 抹成 path，
  提交前 `git restore Cargo.lock`。

