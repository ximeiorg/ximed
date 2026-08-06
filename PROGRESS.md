# PROGRESS

> 更新：2026-08-06 · 阶段：P6 + P7 全部完成（未提交）

## 当前状态

- **P6 部署优化完成**：release profile + 交叉编译配置 + systemd + 内存 benchmark
- **P7 全部完成**：HTTPS / WebDAV / 历史记录 / 图片文件同步
- **56 个测试全绿**（domain 12 + store 12 + service 19 + server 16）；无 openssl/native-tls/openssl-sys；无编译警告
- release 二进制 1.5MB，实测内存 1MB（目标 < 20MB）

## 下一步

1. 代码审查（未提交变更）后提交
2. P5（Xime 宿主扩展 + 采集插件 + 端到端）待用户确认是否实施
3. 可选：S3 存储后端（feature，大依赖按需）

## 已完成

- [x] P1-P4 + 分层重构（domain/store/service/server 四层单向依赖）
- [x] **P6 部署优化**：
  - workspace `[profile.release]`：opt-level=z / lto / codegen-units=1 / strip / panic=abort
  - `.cargo/config.toml`：交叉编译 alias（build-rpi / build-rpi-armv7）+ musl-gcc linker
  - `deploy/ximed.service`：systemd（Restart=always、MemoryMax、资源/安全限制、journald）
  - `deploy/bench_memory.sh`：内存 benchmark（实测 RSS 1MB）
- [x] **P7a HTTPS**：axum-server + rustls（参考官方 example-tls-rustls），证书/私钥经 `XIMED_TLS_CERT`/`XIMED_TLS_KEY` 环境变量注入（不落盘）；tls.enabled 切换；端到端验证（healthz/PUT/SSE 全走 HTTPS）
- [x] **P7b WebDAV**：`store` crate webdav feature（reqwest rustls，无 openssl）；`WebDavStorage` 实现 Storage trait（PUT/GET/DELETE/PROPFIND + MKCOL 建目录 + Basic Auth + url 解码）；`BackendKind`/`StorageOptions` 装配
- [x] **P7c 历史记录**：`store` crate history feature（rusqlite bundled 手写 SQL，决策 9）；`HistoryRepo`（Mutex<Connection> 保证 Send+Sync）；service PUT 时写入；`GET /api/clipboard/history?limit=&before_seq=` 分页倒序；端到端验证（3 条倒序 + 分页 + 401）
- [x] **P7d 图片/文件同步**：domain `validate_file_name`（防路径穿越）；service `put_file`/`get_file`（files/{name} 键空间）；WS 协议扩展 `file.set`/`file.get` + binary 帧上传（pending_file 状态）；`GET /api/clipboard/file?name=` 下载；端到端验证（WS binary 上传 → 落盘 → HTTP 下载字节一致）

## 待办（后续功能点）

- [ ] 审查未提交变更后 GPG 提交
- [ ] P5 Xime 宿主扩展 + 采集插件 + 端到端（待确认）
- [ ] S3 存储后端（按需，大依赖）
