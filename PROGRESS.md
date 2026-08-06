# PROGRESS

> 更新：2026-08-06 · 阶段：P1 完成（ximed-core）

## 当前状态

- **P1 已完成**：workspace + ximed-core 全部模块 + 单元测试（23 个全绿）
- 依赖树无 openssl/openssl-sys/native-tls（`cargo tree -i` 均无结果）
- `cargo check --workspace` 通过

## 下一步

1. **P2：服务器骨架**（crates/ximed）
   - config-rs 配置：default.toml 内嵌 + MemorySource + 环境变量（XIMED_*）+ 多路径文件（./ximed.toml → /app/ximed.toml → /etc/ximed/config.toml）+ Settings 结构 + 访问器
   - Basic Auth（三通道共用校验逻辑）
   - 路由：`/healthz` `/api/version` + `/api/clipboard` GET/PUT（轮询通道先可用）
   - 验收：`cargo test` + curl 全链路 + 配置解析单测
2. P3：WS 通道；P4：SSE 通道

## 已完成

- [x] 技术方案文档定稿 v3（三通道并存：Xime 走 HTTP，WS/SSE 可选）
- [x] 配置设计采用"内嵌默认值 + 环境变量 + 多路径文件"范式（MemorySource + config-rs）
- [x] P1 workspace：根 Cargo.toml = 虚拟 workspace（members = ["crates/*"]，workspace.package：version 0.1.0 / edition 2024 / license MIT）
- [x] P1 hash.rs：`sha256_hex` / `text_hash` / `file_hash`，NIST 向量 + "你好" 向量（670d9743...）+ 通用性断言
- [x] P1 profile.rs：`Profile`（8 字段全小写）+ `ProfileType`（text/file/image/group/none 小写序列化）+ `from_text`/`empty`/`Default`
- [x] P1 protocol.rs：`ClientMessage` / `ServerMessage`（serde tag="type"，rename_all snake_case + 显式 rename 处理 auth.ok/auth.error/clipboard.set/get/changed/snapshot）+ `sse_event` 常量
- [x] P1 storage.rs：`Storage` trait（async_trait，put/get/delete/list）+ `validate_key`（拒绝空/绝对/..//../\\/NUL）+ `LocalStorage`（原子写 tmp+rename，递归 list）
- [x] 依赖清单：ximed-core = async-trait + serde + serde_json + sha2 + tokio(fs,sync)；dev = tempfile + tokio(rt,macros)

## 待办（后续功能点）

- [ ] P2 服务器骨架
- [ ] P3 WS 通道
- [ ] P4 SSE 通道
- [ ] P5 Xime 宿主扩展 + 采集插件 + 端到端
- [ ] P6 部署优化（musl/交叉编译/systemd/内存 benchmark）
- [ ] P7 WebDAV/S3 后端 + 历史记录 + 图片/文件同步 + HTTPS
