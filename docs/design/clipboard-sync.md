# ximed 技术方案：输入法剪贴板同步服务

> 状态：已确认 · 日期：2026-08-06 · 修订：v3（三通道并存：Xime 走 HTTP，其他应用可选用 WS/SSE）

## 1. 背景与目标

**ximed** 是一个用 Rust + axum 实现的输入法数据同步服务。首个功能：**跨设备剪贴板同步**——Xime（Android 输入法）作为客户端，通过 Lua 采集插件同步剪贴板文本到多台设备。

**硬性约束**：
- 支持极低性能设备部署（树莓派、边缘设备）：内存小、磁盘小、弱网
- 依赖**不得直接或间接使用 openssl**
- workspace 模式，每次只做一个功能点，单元测试
- 服务器提供 **HTTP / WebSocket / SSE 三种通道并存**；Xime 手机端只接入 HTTP（省电），其他应用可选用 WS/SSE

## 2. 架构总览

```
┌──────────────────────────┐        ┌──────────────────────────┐
│  Xime 设备 A（Android）    │        │  Xime 设备 B（Android）    │
│  ┌──────────────────────┐ │        │  ┌──────────────────────┐ │
│  │ ClipboardManager     │ │        │  │ ClipboardManager     │ │
│  │ (本地剪贴板历史,已有)  │ │        │  │ (本地剪贴板历史,已有)  │ │
│  └──────────┬───────────┘ │        │  └──────────┬───────────┘ │
│             │ 复制文本    │ │        │             │             │
│             ▼            │ │        │             ▲             │
│  ┌──────────────────────┐ │        │  ┌──────────────────────┐ │
│  │ SyncClipboard 采集插件│ │        │  │ SyncClipboard 采集插件│ │
│  │ (Lua: HTTP 事件驱动)  │ │        │  │ (Lua: HTTP 事件驱动)  │ │
│  │  host.clipboard      │ │        │  │  host.clipboard      │ │
│  │  host.hash           │ │        │  │  host.hash           │ │
│  │  host.http           │ │        │  │  host.http           │ │
│  └──────────┬───────────┘ │        │  └──────────┬───────────┘ │
└─────────────┼────────────┘        └─────────────┼────────────┘
              │ HTTP（事件触发，仅 Xime 接入）       │
              ▼                                    ▲
      ┌───────────────────────────────────────────────┐
      │              ximed 服务器 (Rust/axum)          │
      │                                               │
      │  通道：HTTP REST（主） + WS /sync + SSE /events │
      │         │ WS/SSE 供桌面端/其他实时客户端选用    │
      │  认证：Basic Auth（三通道共用）                 │
      │  广播：tokio::sync::broadcast                  │
      │  存储：Storage trait（可插拔后端）              │
      │   ├── LocalStorage（文件系统，默认）            │
      │   ├── WebDavStorage（坚果云/Nextcloud，可选）   │
      │   └── S3Storage（S3/MinIO/R2，可选）            │
      └───────────────────────────────────────────────┘
```

## 3. 协议设计

### 3.1 三通道并存（HTTP / WebSocket / SSE）

服务器同时提供三种通道，**共享同一剪贴板数据模型与广播源**，客户端按自身能力与场景选择，不强制降级链：

```
HTTP /api/*        事件触发式同步（Xime 手机端使用，最省电）
WebSocket /sync    双向长连接，实时推送（桌面端/需即时同步的应用）
SSE     /events    单向推送，上传走 HTTP（浏览器/单向监听应用）
```

**Xime（手机）**：只接入 HTTP，事件驱动（复制时上传、IME 唤醒时拉取），无长连接、无心跳、无定时器——最省电且符合 Android 10+ 后台剪贴板访问限制。

**其他应用**（桌面客户端、第三方工具、未来 Web 面板等）：可按需选用 WS（双向实时）或 SSE（单向推送），服务器侧三通道共享 `AppState` 与 `broadcast`，无状态差异。

### 3.2 WebSocket 通道 `/sync`（实时客户端选用）

供桌面端/第三方应用使用的双向长连接通道。连接建立时带 `Authorization: Basic <base64(user:pass)>`。连接后首帧发 `auth` 确认。

**客户端 → 服务器（JSON 文本帧）**：

```json
{ "type": "auth", "user": "alice", "token": "xxx" }

{ "type": "clipboard.set",
  "data": {
    "type": "text",
    "hash": "9f86d081884c7d65...",
    "text": "完整文本内容",
    "has_data": false,
    "data_name": null,
    "size": 12,
    "source": "device-a"
  }}

{ "type": "clipboard.get" }
```

**服务器 → 客户端（JSON 文本帧）**：

```json
{ "type": "auth.ok", "version": "0.1.0" }
{ "type": "auth.error", "reason": "invalid credentials" }

{ "type": "clipboard.changed",
  "data": { "type": "text", "hash": "...", "size": 12, "source": "device-a" }}

{ "type": "clipboard.snapshot", "data": { ...完整 profile... } }

{ "type": "ping" }   // 客户端30s一次，服务器60s无消息判定离线
```

### 3.3 SSE 通道 `/events`（单向监听客户端选用）

供浏览器/单向监听应用使用的推送通道，上传仍走 HTTP：

```
GET /events     →  text/event-stream
    event: clipboard
    id: 42
    data: {"type":"text","hash":"...","size":12,"source":"device-a"}

    event: ping
    data: {}
```

- 客户端收到 `clipboard` 事件 → 对比本地 hash → 需要时 `PUT /api/clipboard` 上传或 `GET /api/clipboard` 拉取
- 断线重连：SSE 标准 `Last-Event-ID` 头回传 `id`，服务器从该事件之后继续（需要服务端事件序号存储，P3 实现内存序号即可）
- 心跳：服务器每 15s 发 `ping` 事件保活

### 3.4 HTTP REST API（主通道：Xime 手机端）

```
GET  /api/clipboard             → 200 { profile } | 304（If-None-Match 命中）
PUT  /api/clipboard             → 上传 profile
GET  /api/clipboard/history     → 历史记录（P6）
GET  /events                    → SSE 流（其他应用用）
GET  /healthz                   → 200 { "status": "ok" }
GET  /api/version               → { "version": "0.1.0" }
```

**Xime 事件驱动同步流程**（无长连接、无定时器）：

```
本地剪贴板变化（用户复制）
  → IME 前台：PUT /api/clipboard 上传（带 Basic Auth，If-Match 或 hash 幂等）
接收端 IME 唤醒（键盘弹出/打开剪贴板面板）
  → GET /api/clipboard，带 If-None-Match: <本地hash>
  → 304 无变化 / 200 有变化（含完整文本）→ 对比 hash → host.clipboard.write(text)
```

### 3.5 去重规则（三通道统一）

- 接收方收到变更（HTTP 拉取 / WS / SSE 推送）→ 与本地 hash 对比 → 不同才写入 `host.clipboard.write(text)`
- 服务器对相同 hash 的 `clipboard.set` 幂等丢弃（不重复广播）

## 4. 数据模型

### 4.1 Profile（JSON 字段全小写）

```rust
// crates/ximed-core/src/profile.rs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub r#type: ProfileType,          // text | file | image | group | none
    pub hash: String,                 // 小写 hex SHA256
    pub text: String,                 // 文本内容或预览
    pub has_data: bool,               // 是否有附件数据文件
    pub data_name: Option<String>,    // 附件文件名（后续图片/文件）
    pub size: usize,
    pub source: Option<String>,       // 来源设备标识
}

pub enum ProfileType { Text, File, Image, Group, None }
```

> **命名约定：所有字段一律小写**（snake_case，`has_data`/`data_name`），JSON 字段、Rust 字段、数据库列名统一小写，不使用大写开头、不使用 camelCase。枚举值序列化为小写（`"type": "text"`）。

### 4.2 Hash 计算（对齐 SyncClipboard Hash.md 算法）

```
text:  SHA256(utf8(text))，hex（小写）
file:  SHA256(utf8("filename|" + SHA256(content) hex))，hex（小写）  // 后续
```

```rust
// crates/ximed-core/src/hash.rs
pub fn sha256_hex(data: &[u8]) -> String  // sha2 crate，纯 Rust
pub fn text_hash(text: &str) -> String
```

**测试向量**：用 SyncClipboard 已知输入输出（仅取算法结果，协议字段大小写无关）做单元测试。

## 5. 存储设计（抽象层 + 可插拔后端）

### 5.1 Storage trait（核心抽象）

**决策：存储抽象为键值 Blob 接口**，所有后端（本地文件 / WebDAV / S3 / Turso）实现同一 trait，服务器业务代码只依赖 trait，不关心后端。

```rust
// crates/ximed-core/src/storage.rs
#[async_trait]
pub trait Storage: Send + Sync {
    /// 写入 key → blob（覆盖）
    async fn put(&self, key: &str, data: &[u8]) -> Result<()>;
    /// 读取 key，不存在返回 None
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    /// 删除 key（幂等）
    async fn delete(&self, key: &str) -> Result<()>;
    /// 列出 prefix 下的 key
    async fn list(&self, prefix: &str) -> Result<Vec<String>>;
}
```

**键空间约定**（后端无关，S3/WebDAV 映射为 bucket/目录前缀）：

```
clipboard/current.json   # 当前剪贴板 profile（三通道共享的唯一事实源）
files/{data_name}      # 附件数据（后续图片/文件）
history/{id}.json        # 历史记录（P7，可换 Turso）
```

### 5.2 后端实现（按 Cargo feature 可选编译）

| 后端 | Crate/实现 | 说明 | 依赖体积 |
|---|---|---|---|
| `LocalStorage`（默认） | 文件系统 | `{data_dir}/{key}`，原子写（tmp+rename） | 零新增 |
| `WebDavStorage`（feature） | reqwest(rustls) + 手写 PUT/GET/MKCOL/PROPFIND | 坚果云、Nextcloud、NAS；HTTP 语义天然契合 | 中 |
| `S3Storage`（feature） | S3 兼容 API | AWS S3 / MinIO / Cloudflare R2；SigV4 签名 | 大（按需） |
| `TursoStorage`（feature） | libsql | 基于 libSQL（SQLite fork）：本地嵌入式文件 或 Turso 远程云库（Hrana）；结构化历史、查询 | 中 |

```toml
# crates/ximed/Cargo.toml
[features]
default = ["local"]
local   = []                                   # 文件系统（零依赖）
webdav  = ["dep:reqwest", "rustls-tls"]        # 可选 WebDAV
s3      = ["dep:aws-sdk-s3", "dep:aws-config"] # 可选 S3
turso   = ["dep:libsql"]                       # 可选 Turso（历史记录）

[dependencies]
aws-sdk-s3 = { version = "1", optional = true, default-features = false,
               features = ["rustls"] }         # 无 openssl
libsql     = { version = "0.6", optional = true }  # libSQL 客户端：本地文件 / Turso 远程（Hrana）
reqwest    = { version = "0.12", optional = true, default-features = false,
               features = ["rustls-tls", "json"] }
```

> 注意：`aws-sdk-s3` 体积大、编译慢，**不放进 default**。树莓派默认部署 = 本地文件，S3/WebDAV 仅在确实需要远程存储时开启。

### 5.3 本地默认实现（P2 落地）

- 当前剪贴板：`clipboard/current.json`，内存缓存 `Arc<RwLock<Option<Profile>>>` + `Mutex` 串行化写入 + 原子写
- 不做数据库起步——当前功能点只有单条剪贴板，DB 是过度设计；`TursoStorage` 留到历史记录（P7）

## 5.4 配置设计（config-rs）

**决策：使用 [`config`](https://crates.io/crates/config)（config-rs）库统一管理配置**，采用"内嵌默认值 + 环境变量 + 多路径文件"范式（参考成熟项目实践），实测 config 0.15 不引入 openssl/native-tls。

```toml
# crates/ximed/Cargo.toml
[dependencies]
config = { version = "0.15", default-features = true }  # toml/yaml/json/env
```

**加载优先级**（Builder 按序 `add_source`，后添加者优先）：

```
1. 内嵌默认值   MemorySource::new()  ← include_str!("../../default.toml")
2. 环境变量     Environment::default().try_parsing(true)   XIMED_* 前缀
3. 配置文件     ./ximed.toml（required(false)）
4. 配置文件     /app/ximed.toml（required(false)）
5. 配置文件     /etc/ximed/config.toml（required(false)）  ← 最高优先级
```

### 5.5 配置实现（Settings 结构 + MemorySource 范式）

```rust
// crates/ximed/src/config.rs
use config::{Config, ConfigError, File, FileFormat, Map, Source, Value, Environment};

/// 内嵌默认配置源：把 default.toml 编译进二进制，保证裸跑可启动。
#[derive(Debug, Clone)]
struct MemorySource {
    content: String,
}

impl MemorySource {
    pub fn new() -> Self {
        Self {
            content: include_str!("../../default.toml").to_string(),
        }
    }
}

impl Source for MemorySource {
    fn clone_into_box(&self) -> Box<dyn Source + Send + Sync> {
        Box::new((*self).clone())
    }

    fn collect(&self) -> Result<Map<String, Value>, ConfigError> {
        FileFormat::Toml
            .parse(None, self.content.as_str())
            .map_err(|cause| ConfigError::FileParse { uri: None, cause })
    }
}

#[derive(Debug, Deserialize)]
pub struct Settings {
    pub server: ServerConfig,
    pub auth: AuthConfig,
    pub clipboard: ClipboardConfig,
    #[serde(default)] pub http: HttpConfig,
    #[serde(default)] pub ws: WsConfig,
    #[serde(default)] pub sse: SseConfig,
    pub storage: StorageConfig,
    #[serde(default)] pub tls: TlsConfig,
}

impl Settings {
    pub fn new() -> Result<Self, ConfigError> {
        let s = Config::builder()
            .add_source(MemorySource::new())          // 1. 内嵌默认值
            .add_source(
                Environment::default().prefix("XIMED").try_parsing(true)  // 2. 环境变量
            )
            .add_source(File::with_name("ximed.toml").required(false))        // 3. 当前目录
            .add_source(File::with_name("/app/ximed.toml").required(false))   // 4. /app
            .add_source(File::with_name("/etc/ximed/config.toml").required(false)) // 5. /etc
            .build()?;
        s.try_deserialize()
    }

    // 访问器（供各处安全取值）
    pub fn server(&self) -> &ServerConfig { &self.server }
    pub fn auth(&self) -> &AuthConfig { &self.auth }
    pub fn clipboard(&self) -> &ClipboardConfig { &self.clipboard }
    pub fn http(&self) -> &HttpConfig { &self.http }
    pub fn ws(&self) -> &WsConfig { &self.ws }
    pub fn sse(&self) -> &SseConfig { &self.sse }
    pub fn storage(&self) -> &StorageConfig { &self.storage }
    pub fn tls(&self) -> &TlsConfig { &self.tls }
}

/// 默认值函数（serde default 使用）
fn default_addr() -> String { "0.0.0.0:8443".to_string() }
fn default_log_level() -> String { "info".to_string() }
fn default_max_frame_size() -> usize { 1024 * 1024 }
```

```rust
#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_addr")]
    pub addr: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    pub workers: Option<usize>,                       // null = 按 CPU
    #[serde(default = "default_log_level")]
    pub log_level: String,
}
// ... AuthConfig / ClipboardConfig / HttpConfig / WsConfig / SseConfig /
//     StorageConfig（enum + 各后端字段）/ TlsConfig 同上，含 Default
//     每个默认值用 fn default_xxx() 独立定义
```

**目录结构**（default.toml 内嵌路径约定）：

```
crates/ximed/
├── default.toml          # 内嵌默认配置（include_str! 编译进二进制）
└── src/
    └── config.rs         # Settings + MemorySource + 访问器
```

### 5.6 配置项完整定义（default.toml）

```toml
# crates/ximed/default.toml —— 全部配置项及默认值（内嵌到二进制）

[server]
addr = "0.0.0.0:8443"          # 监听地址
data_dir = "./data"             # 数据目录（存储后端用）
workers = null                  # tokio worker 线程数（null = 按 CPU，低端设备可设 1）
log_level = "info"              # trace/debug/info/warn/error

[auth]
username = "alice"              # Basic Auth 用户名
password_env = "XIMED_AUTH_PASSWORD" # 密码从该环境变量读取（必填项，未设置则拒绝启动）
max_connections = 32            # 最大并发连接（WS/SSE 合计），防资源耗尽

[clipboard]
max_frame_size = 1048576        # 单帧/单条上限（字节，默认 1MB）
idle_timeout_secs = 60          # 长连接无消息判定离线（秒）
heartbeat_interval_secs = 30    # 客户端心跳间隔（秒，客户端按此值发 ping）
default_profile = { type = "text", hash = "", text = "", has_data = false,
                    data_name = null, size = 0, source = null }  # 空剪贴板初始值

[http]
enabled = true                  # 是否启用 HTTP REST 通道（Xime 主通道）
bind_timeout_ms = 5000          # 单请求超时（毫秒）

[ws]
enabled = true                  # 是否启用 WebSocket 通道
path = "/sync"                  # WS 端点路径

[sse]
enabled = true                  # 是否启用 SSE 通道
path = "/events"                # SSE 端点路径
heartbeat_ms = 15000            # SSE 服务器 ping 间隔（毫秒）

[storage]
backend = "local"               # local | webdav | s3 | turso
# --- webdav 后端（feature = webdav 时启用）---
# url = "https://dav.jianguoyun.com/dav/xime"
# username = ".."
# password_env = "XIMED_WEBDAV_PASSWORD"
# --- s3 后端（feature = s3 时启用）---
# endpoint = "https://s3.example.com"
# bucket = "xime"
# region = "us-east-1"
# access_key_env = "XIMED_S3_ACCESS_KEY"
# secret_key_env = "XIMED_S3_SECRET_KEY"
# --- turso 后端（feature = turso 时启用，P7）---
# url = "file://data.db"        # 本地嵌入式（SQLite 兼容）或 "libsql://<db>.turso.io"
# auth_token_env = "XIMED_TURSO_TOKEN"   # 仅远程云库需要

[tls]                            # 未来 HTTPS（feature = tls 时启用，rustls）
enabled = false
cert_env = "XIMED_TLS_CERT"
key_env = "XIMED_TLS_KEY"
```

**敏感项约定**：所有密码/密钥类配置（`auth.password`、webdav/s3/turso 凭据、tls 证书）一律通过 `XIMED_*` 环境变量注入（Environment 源 `prefix("XIMED")` + 双下划线嵌套映射，如 `XIMED_AUTH_PASSWORD`），`ximed.toml` 不落盘明文。环境变量 `try_parsing(true)` 自动把字符串解析为 bool/int 等类型。

## 6. 认证与安全

- **Basic Auth**（无状态、低开销）：HTTP 头 + WS 首帧 + SSE 查询参数（或 cookie）三通道共用同一校验逻辑
- 密码支持从环境变量读取（生产部署避免明文配置文件）
- **WebSocket/SSE 帧大小限制**：单帧上限 1MB，防内存攻击
- 连接数限制（如 32），防资源耗尽（树莓派场景）
- `data_name` 路径校验（拒绝 `\` `/` `..`），防路径穿越
- 未来 HTTPS：rustls（纯 Rust，无 openssl），或前置 nginx/caddy 终止 TLS

## 7. 服务器技术栈与依赖清单

```toml
# crates/ximed/Cargo.toml
[dependencies]
axum = { version = "0.8", features = ["ws", "tokio", "sse"] }
tokio = { version = "1", features = ["rt-multi-thread", "net", "macros", "signal", "time", "sync"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"                # hash，纯 Rust
base64 = "0.22"              # Basic Auth 解析
config = { version = "0.15" } # 配置加载（toml/yaml/json/env 源，无 openssl）
tokio-rustls = "0.26"        # 仅未来 HTTPS 需要
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

[dev-dependencies]
tokio = { version = "1", features = ["rt", "macros"] }
```

**验证无 openssl 依赖**：`cargo tree -i openssl` 应无结果；CI 增加此检查（含各 feature 组合）。

## 8. Workspace 结构

```
ximed/
├── Cargo.toml                 # [workspace] members = ["crates/*"]
├── crates/
│   ├── ximed/                 # 服务器二进制
│   │   ├── default.toml       # 内嵌默认配置（include_str! 编译进二进制）
│   │   ├── src/main.rs        # 启动、路由装配、通道选择
│   │   ├── src/auth.rs        # Basic Auth 校验（三通道共用）
│   │   ├── src/ws.rs          # WS 通道处理
│   │   ├── src/sse.rs         # SSE 通道处理
│   │   ├── src/http.rs        # HTTP REST / 轮询通道
│   │   ├── src/store.rs       # 存储装配（按 feature 选择后端）
│   │   └── src/config.rs      # Settings + MemorySource + 多路径文件 + 环境变量
│   └── ximed-core/            # 共享库
│       ├── src/profile.rs     # Profile 类型
│       ├── src/hash.rs        # SHA256 hash 计算
│       ├── src/protocol.rs    # WS/SSE/HTTP 消息帧类型
│       ├── src/storage.rs     # Storage trait + LocalStorage
│       └── src/lib.rs
```

```
应用层状态：
┌─────────────────────────────────────────┐
│ AppState                                │
│  broadcast: broadcast::Sender<Profile>  // 变更广播（三通道共用）
│  current:   Arc<RwLock<Option<Profile>>>// 当前剪贴板缓存
│  store:     Arc<dyn Storage>            // 抽象存储（后端可插拔）
│  auth:      Arc<AuthConfig>             // 认证配置
│  seq:       AtomicU64                   // 事件序号（SSE Last-Event-ID）
└─────────────────────────────────────────┘
```

## 9. Xime 侧集成设计（最小改动清单）

因为客户端功能属于 Xime 项目，这里给出**采集插件所需的最小宿主扩展**：

| 新增能力 | 实现位置 | 说明 |
|---|---|---|
| `host.clipboard.get()` | app 层 `ClipboardHostApiImpl` | 桥接 `ClipboardManager.getCurrentClipboardText()` |
| `host.clipboard.write(text)` | 同上 | 桥接 `copyToSystemClipboard()`；加"忽略自身写入触发监听"标记（`ClipboardOwnerProvider`，对齐 SyncClipboard） |
| `host.clipboard.onChanged(cb)` | 同上 | 订阅 `OnPrimaryClipChangedListener`（复用 `ClipboardManager` 已有监听） |
| `host.hash.sha256(str)` → hex | app 层 | Lua 沙箱无 sha 库，宿主提供原语 |
| `host.http.request(method, url, headers, body)` | 新增 `HttpHostApi`（plugin-core）+ app 实现 | Xime 唯一网络原语：上传/拉取/带 If-None-Match；OkHttp 通用，复用 `NetworkPolicy` 域名校验 |
| 新插件分类 `clipboard`（或 `sync`） | `PluginCategory` | `Activation.MULTI` 或 `SINGLE` |
| 采集插件 `plugins/sync-clipboard` | 纯 Lua xipk | 事件驱动：onChanged→hash→HTTP 上传；IME 唤醒→HTTP 拉取→对比→写入 |
| 域名授权 | 复用现有 `NetworkPolicy` | manifest 声明 ximed 服务器域名（http/https），用户授权一次 |

**Xime 插件同步逻辑（HTTP 事件驱动，无长连接）**：

```
本地剪贴板变化（用户复制）
  → host.clipboard.onChanged 回调 Lua
  → hash = host.hash.sha256(text)
  → host.http.request(PUT /api/clipboard, {profile})     // 前台一次请求

IME 被唤醒（键盘弹出/打开剪贴板面板）
  → host.http.request(GET /api/clipboard, If-None-Match: 本地hash)
  → 200 → 对比 hash → 不同 → host.clipboard.write(text)
```

**端到端流程**：

```
设备A 用户复制 "你好"
  → ClipboardManager.onChanged → host.clipboard.onChanged 回调 Lua
  → Lua: hash = host.hash.sha256(text)
  → host.http.request(PUT /api/clipboard, {...})   // 事件触发，仅一次
设备B 用户键盘弹出（IME 唤醒）
  → host.http.request(GET /api/clipboard, If-None-Match: 本地hash)
  → 200 变化 → 对比 hash 不同 → host.clipboard.write("你好")
```

## 10. 分阶段实施计划（遵循"每次一个功能点"）

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P1** | workspace + `ximed-core`：Profile/hash/protocol + Storage trait + LocalStorage + 单元测试 | `cargo test` 全绿，hash 对齐测试向量 |
| **P2** | 服务器骨架：config-rs 配置（default.toml 内嵌 + MemorySource + 环境变量 + 多路径文件 + Settings 访问器）+ Basic Auth + 路由 + `/healthz` `/api/version` + `/api/clipboard` GET/PUT（轮询通道先可用） | `cargo test`；curl 验证全链路；配置解析单测 |
| **P3** | WS 通道：认证 + `clipboard.set/get` + broadcast 广播 + 心跳/超时 + 幂等去重 | 两个 websocat 客户端互发验证广播；单测帧解析 |
| **P4** | SSE 通道：`/events` + 事件序号 + Last-Event-ID 续传 + 三通道共享广播 | curl 验证 SSE 流；单测序号逻辑 |
| **P5** | Xime 宿主扩展（clipboard/hash/sse/http 原语）+ 采集插件 + 通道降级状态机 + 端到端联调 | 设备 A 复制 → 设备 B 剪贴板更新；断 WS 自动降级 SSE/轮询 |
| **P6** | 部署优化：release profile（lto/opt-level=z）、musl 交叉编译 aarch64/armv7、systemd unit、内存 benchmark | 树莓派可运行，内存 < 20MB |
| **P7（后续）** | WebDAV/S3 存储后端（feature 开关）、历史记录（Turso/libsql）、图片/文件同步（binary WS 帧）、HTTPS（rustls） | 按需 |

**每个阶段独立提交（GPG 签名），功能点之间不做重构。**

## 11. 低性能部署优化

```toml
# .cargo/config.toml 交叉编译（树莓派）
[build]
target = "aarch64-unknown-linux-musl"

[profile.release]
opt-level = "z"
lto = true
codegen-units = 1
strip = true
panic = "abort"
```

- 运行内存目标：**< 20MB**（默认 local 后端，预计 5-10MB 起步）
- 可选：`#[tokio::main(flavor = "current_thread")]` 单线程 runtime（树莓派零核场景）
- systemd service：`Restart=always`、`LimitNOFILE`、日志走 journald
- Docker 可选：`alpine + musl` 静态二进制，无 openssl，镜像 < 5MB
- S3/WebDAV feature 默认关闭，避免引入大依赖拖累低端设备构建

## 12. 测试策略

- **单元测试**（P1-P4）：hash 向量、Profile 序列化（小写字段）、WS/SSE 帧解析、认证拒绝、幂等去重、Last-Event-ID 序号、**配置解析（config-rs：默认值/配置文件/环境变量覆盖/敏感项缺失校验）**
- **存储测试**：`Storage` trait 用 mock 后端验证业务；LocalStorage 原子写/并发写
- **集成测试**（P3-P4）：内存中起服务器，两个 tokio 客户端验证 WS 与 SSE 广播链路
- **端到端**（P5）：Xime 真机/模拟器，含通道降级切换
- **CI 检查**：`cargo test` + `cargo tree -i openssl`（含 feature 组合）断言无输出 + `cargo check`

## 13. 关键决策记录（DECISIONS.md 候选）

1. **所有字段一律小写**：JSON 字段、Rust 结构体字段、数据库列名统一 snake_case 小写（`has_data`/`data_name`），不使用大写开头、不使用 camelCase、不使用 PascalCase；枚举值序列化为小写（`"type": "text"`）。放弃与 SyncClipboard 字面协议兼容（其字段为 PascalCase），保留 Profile/Hash 算法概念；若未来需对接 SyncClipboard 桌面客户端，在协议适配层做大小写转换
2. **三通道并存（HTTP 主 / WS / SSE 可选）**：Xime 手机端只接入 HTTP 事件驱动同步（零长连接、最省电、符合 Android 10+ 后台剪贴板访问限制）；WS/SSE 供未来桌面端/浏览器等其他实时客户端选用，不强制降级链
3. **存储抽象为 `Storage` trait**：本地文件（默认）/ WebDAV / S3 / Turso 可插拔，业务代码不感知后端；Cargo feature 控制依赖体积
4. **当前剪贴板单条数据用文件存储而非数据库**：DB 是过度设计；`TursoStorage`（libsql）留到历史记录（P7）
5. **Basic Auth 而非 token**：无状态、低开销、三通道共用；未来可平滑升级
6. **HTTPS 用 rustls**：硬性规则禁止 openssl；或前置反向代理
7. **S3/WebDAV 为可选 feature**：默认 local，避免 aws-sdk 大依赖拖累低端设备
8. **不引入 MQTT**：Xime 手机端已采用 HTTP 事件触发（零长连接，最省电），MQTT 的省电优势建立在常驻连接之上，反而更费；未来若出现真实"实时推送"客户端需求，再评估 MQTT 替代 WS/SSE（当前阶段不做）
9. **历史记录用 `libsql` SDK 手写 SQL，不引入 ORM**：调研确认 toasty 是目前唯一原生提供 Turso driver 的 ORM（`toasty-driver-turso`），但 toasty 0.9.0 较新、API 不稳定；本地嵌入式 Turso 实际是 SQLite 兼容，sea-orm/sqlx/diesel 均可直接用 SQLite driver；而历史记录查询复杂度低（时间倒序+分页），libsql 手写 SQL 足够，ORM 帮助不大。若未来需对接 Turso 云库/复杂查询，再评估 toasty
10. **配置用 config-rs 统一管理，采用"内嵌默认值 + 环境变量 + 多路径文件"范式**：`MemorySource::new()`（`include_str!` 编译内嵌 `default.toml`）→ `Environment::default().prefix("XIMED").try_parsing(true)` → `./ximed.toml` → `/app/ximed.toml` → `/etc/ximed/config.toml`（后添加者优先）；`Settings` 结构 + 访问器方法；敏感项（认证密码、存储凭据、TLS 证书）一律 `XIMED_*` 环境变量注入不落盘明文；实测 config 0.15 不引入 openssl/native-tls
