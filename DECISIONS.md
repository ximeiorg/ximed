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
- **许可证待决**：`xime-sync-domain` 现为 GPL-3.0-or-later（原来在 libximecore 里是
  MIT），libximep2p 的 `xime-p2p` 同为 GPL。ximed 目前是 MIT——接入 p2p 后整体分发
  必须转 GPL-3.0-or-later（与 XimeYi / Android 一致），尚未改动 LICENSE。

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

