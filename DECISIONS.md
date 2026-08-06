# DECISIONS

> 重要决策记录，来源：docs/design/clipboard-sync.md 第 13 节

## 1. 所有字段一律小写

JSON 字段、Rust 结构体字段、数据库列名统一 snake_case 小写（`has_data`/`data_name`），不使用大写开头、不使用 camelCase、不使用 PascalCase；枚举值序列化为小写（`"type": "text"`）。放弃与 SyncClipboard 字面协议兼容（其字段为 PascalCase），保留 Profile/Hash 算法概念；若未来需对接 SyncClipboard 桌面客户端，在协议适配层做大小写转换。

## 2. 三通道并存（HTTP 主 / WS / SSE 可选）

Xime 手机端只接入 HTTP 事件驱动同步（零长连接、最省电、符合 Android 10+ 后台剪贴板访问限制）；WS/SSE 供未来桌面端/浏览器等其他实时客户端选用，不强制降级链。

## 3. 存储抽象为 Storage trait

本地文件（默认）/ WebDAV / S3 / Turso 可插拔，业务代码不感知后端；Cargo feature 控制依赖体积。

## 4. 当前剪贴板单条数据用文件存储而非数据库

DB 是过度设计；`TursoStorage`（libsql）留到历史记录（P7）。

## 5. Basic Auth 而非 token

无状态、低开销、三通道共用；未来可平滑升级。

## 6. HTTPS 用 rustls

硬性规则禁止 openssl；或前置反向代理。

## 7. S3/WebDAV 为可选 feature

默认 local，避免 aws-sdk 大依赖拖累低端设备。

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
