# PROGRESS

> 更新：2026-08-06 · 阶段：三形态架构 + 发布打包（CI nightly/release）实现完成（未提交）

## 当前状态

- **三形态架构完成**：
  - Form 1 `ximed-server` 二进制：仅服务器，release ~3MB
  - Form 2 `ximed-desktop` 二进制：同进程 client + iced 窗口 + 托盘 + **受控 server**（不自动启动，UI 手动启停）；**零配置启动**（未配密码时自动生成随机密码落盘 `{data_dir}/auth_password`，权限 0600，client 复用）
  - Form 3 `ximed-client` 二进制：仅客户端 daemon + 托盘（默认构建）；无 UI 构建下退化为纯 daemon
  - `crates/app` 已重构为 **lib + 三个 bin**（`src/bin/ximed-server.rs` / `ximed-desktop.rs`（required-features=tray）/ `ximed-client.rs`）；`--no-default-features` 只出 server + 纯 daemon client
- **发布打包（CI）**：
  - `.github/workflows/build-packages.yml`（可复用，`publish` 输入：none/tag/nightly）：Windows x86_64 + Linux x86_64/aarch64/armv7(musl) + macOS aarch64/x86_64 矩阵
  - server 包 = 裸二进制归档（zip/tar.gz + systemd unit）；desktop 包 = 桌面包格式（Windows NSIS setup.exe + 便携 zip / Linux deb + AppImage / macOS dmg）
  - 桌面包由 **cargo-packager**（Tauri 同源）生成，配置在 `[package.metadata.packager]`（crates/app/Cargo.toml）
  - Windows exe 图标：`assets/icon.ico`（多尺寸）+ build.rs/embed-resource 嵌入 PE 资源
  - `.github/workflows/nightly.yml`：每日 02:00 UTC + 手动；产物文件名带构建日期；`publish=nightly` 删除旧并重建**滚动 nightly release**（始终一个，含最新全部包）
  - `.github/workflows/release.yml`：v* tag 触发，`publish=tag` 发布版本 GitHub Release
  - `.github/workflows/ci.yml`：每次 push/PR 检查（fmt + clippy -D warnings 默认与无默认 feature + 三平台全量测试）
- **UI 左右分栏**（iced）：左侧导航（状态/剪贴板历史）+ 右侧内容面板
  - 状态面板：server / engine / 设备状态 + **本地服务器控制区**（仅 desktop 显示：开启/重启/关闭 + 打开配置文件所在目录）
  - 历史面板：后台线程周期拉取 `/api/clipboard/history`（3s）显示剪贴板历史列表
  - **配置编辑面板已移除**：不再在 UI 中编辑 ximed.toml，改为控制区打开配置文件所在目录（macOS `open` / Linux `xdg-open`）
- **server 受管启动**：`server::run_managed(settings, shutdown_watch)` 支持优雅停止（HTTP `with_graceful_shutdown` / TLS `Handle::graceful_shutdown`），desktop 后台 `server_control_loop` 响应 UI 命令启停；UI 经新 `UiCmd` 通道命令后台
- **密码支持配置文件名直写**（环境变量优先，非必需）：`auth.password` / `[client] password` 字段
- **UI 框架 iced 0.14**：**wgpu（GPU，Metal/Vulkan）优先 + tiny-skia（CPU）自动兜底**（低性能设备仅跑 server，不受影响）；完整构建 32s
- **67 个测试全绿**（含 `run_managed` 优雅停止测试）；无 openssl；无编译警告
- **构建方式（详见设计文档 8.5）**：
  - `cargo build --release -p server` → Form 1（3MB）
  - `cargo build --release -p app` → Form 2/3 统一二进制（含托盘）
  - `cargo build --release -p app --no-default-features` → 精简构建（仅 server + 纯 daemon client）

## 下一步

1. 审查未提交变更后提交
2. 端到端实测：desktop 点「开启」→ 本地 server 启动 → 真实剪贴板复制 → 同步到其他设备
3. 触发一次 CI 验证：nightly 手动跑通，确认各平台桌面包格式产出

## 已完成

- [x] P1-P7 全部（历史记录、WebDAV、HTTPS、文件同步，见下）
- [x] **三形态架构（本次）**：
  - **server 库化**：`crates/server/src/lib.rs` 暴露 `run(settings)` / `run_managed` / `init_tracing` / `check_auth_password`，`main.rs` 变薄入口；行为不变，测试保绿
  - **client crate（新增）**：`ClipboardAdapter`（arboard，跨平台）+ `WsTransport`（tokio-tungstenite Basic Auth）/`HttpTransport`（reqwest rustls 兜底）+ `SyncEngine`（轮询本地 + 远端事件，hash 去重 + 自写回声抑制 + 纯决策函数可单测）；`ClientConfig::load`（config-rs `[client]` 节 + `XIMED_CLIENT_*` env）
  - **app crate（新增）**：`ximed` 统一二进制，clap 子命令 `server` / `client` / `desktop`
  - **desktop 形态**：iced 窗口（主线程）+ 后台 tokio 线程承载 client 引擎 + 历史轮询 + **受控 server**；server 不自动启动，UI 控制区按钮经 `UiCmd` 通道启停
  - **托盘**：tray-icon + muda（`tray` feature 门控）；菜单（状态/显示窗口/退出）+ 左键点击；事件经 proxy 转发 iced
  - **部署文件**：`deploy/ximed.desktop.systemd.service`（桌面 systemd user unit）+ `deploy/com.ximed.desktop.plist`（macOS launchd，含 LSUIElement 说明）+ `deploy/ximed.desktop.toml.example`（桌面配置示例）
- [x] **发布打包（本次）**：
  - **app crate 拆 lib + 三 bin**：`ximed-server` / `ximed-desktop`(required-features tray) / `ximed-client`；server 包 `--no-default-features` 最小体积
  - **桌面图标**：`assets/icon.png`(512) + `assets/icon.ico`(多尺寸)；iced 窗口图标（`window::Settings.icon`）+ Windows exe 图标（build.rs + embed-resource）
  - **cargo-packager 配置**：`[package.metadata.packager]`（deb/nsis 配置），CI 里 `cargo packager --target <triple> --formats <f>` 出 NSIS/deb/AppImage/dmg；`cargo install cargo-packager --locked`
  - **三个 workflow**：`build-packages.yml`（workflow_call 矩阵）/ `nightly.yml`（schedule+manual，产物带日期）/ `release.yml`（v* tag → GitHub Release）
  - 本地已验证：`cargo packager --formats nsis` 产出 `ximed-desktop_0.1.0_x64-setup.exe`；exe 图标资源嵌入确认
- [x] **CI 检查（本次）**：
  - 全仓 `cargo fmt --all` 统一格式（含 CRLF/LF 行尾）
  - 修复 clippy -D warnings 告警：let-chain 折叠嵌套 if（store/local、http、ws、desktop）、`BackendKind::from_str`→`from_name`（避免与 std FromStr 混淆）、无用 `.into()`、`MockClipboard` 补 Default、`SyncMode` derive Default、`if let`/`if` 替换单分支 match、`await_holding_lock`（测试 env_lock 场景 allow）
  - 75 个测试全绿；`cargo check -p app --no-default-features` 通过
- [x] P6 部署优化 / P7a HTTPS / P7b WebDAV / P7c 历史记录 / P7d 文件同步

## 待办（后续功能点）

- [ ] 审查未提交变更后 GPG 提交
- [ ] 真实剪贴板端到端验证（desktop 手动开启 server → 桌面复制 → 手机/另一桌面同步）
- [ ] macOS .app 打包（LSUIElement + 图标）
- [ ] P5 Xime 宿主扩展 + 采集插件（待确认）
- [ ] S3 存储后端（按需，大依赖）

## 关键实现细节（本次会话）

- iced 0.14 函数式 API：`application(boot, update, view)` + `subscription(Fn(&State) -> Subscription)` + `.theme(Fn)`
- 跨线程桥接：std mpsc channel（后台线程 `tx.send`）+ `Subscription::run_with(RxId(rx), fn)` 轮询注入；`RxId` 实现 `Hash + Clone + Eq` 满足 subscription data 约束
- **UI → 后台命令通道**：`UiCmd`（ServerStart/Restart/Stop/OpenConfigDir）经 std mpsc；`server_control_loop` 200ms 轮询响应，watch channel 触发 `server::run_managed` 优雅停止
- tray-icon 0.24 / muda 0.19：`MenuEvent::receiver()` crossbeam channel 任意线程轮询；`Rc<MenuId>` 非 Send，轮询线程只捕获 id 字符串；托盘事件经 UI channel 转发（含 Quit → `UiMsg::Quit` → `iced::exit()`）
- iced 渲染：**wgpu（GPU）优先 + tiny-skia 自动兜底**；tiny-skia 软渲染下 `Shadow` 逐像素计算，大面积容器不用阴影（Retina 下 ~2.5M 物理像素/帧会卡）
- 依赖在 app crate（feature `tray`），server crate 零 UI 依赖，保持低端设备轻量
