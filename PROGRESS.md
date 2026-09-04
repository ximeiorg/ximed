# PROGRESS

> 更新：2026-09-04 · 阶段：同步插件框架（clipboard_sync 插件接入适配器体系 + 同步独立菜单）

## 当前状态

- **同步插件框架（本次）**：复用 libximecore `xime-plugin`（.xipk 安装/Lua 沙箱/clipboard_sync 契约），ximed 侧补齐三块——configSchema 解释器（设置表单数据驱动）、SyncPluginManager（已装插件管理/配置读写/构建通道）、插件商店索引（index.ximei.me 拉取/sha256 校验下载）
- **同步独立菜单（本次二轮调整）**：侧栏「同步」页 = 同步方式（插件单选激活 + 插件 configSchema 动态表单 + 测试连接 + 恢复默认）+ 同步开关卡片 + 插件商店；**原生 server/WebDAV/S3 表单从 UI 移除**（保留为 daemon/ximed.toml 配置能力），设置页随之取消——选中插件即激活，卸载激活插件自动回退本机 server
- **ximed_sync 插件自动填充（本次三轮）**：desktop 后台把本机 server 连接信息（url/username/password，经 `UiMsg::LocalServerInfo` 下发）给 UI；选中 `com.kingzcheung.xime.plugin.ximed_sync` 时空缺的 serverUrl/username/password 字段自动填入本地配置（已保存值不覆盖），三条件单测覆盖（其他插件不填/无本地信息不填/已有值不覆盖）
- **插件成为第四种同步适配器**：`AdapterKind::Plugin` + `ClientConfig{plugins_dir, plugin_id}`；默认仍为本机 server（零配置开箱即用），选插件并保存后切换
- **上游修复（libximecore@1403bff）**：`PluginManifest.config_schema` 缺 `serde(rename = "configSchema")`，真实插件的 camelCase 键从未被反序列化过；已修复并补测试
- 测试全绿（client 60 含插件 feature + app 14）；fmt/clippy 三种 feature 组合干净；`--no-default-features` 最小 server 构建不受影响

## 下一步

1. GUI 手工验证：桌面启动 ximed-desktop → 同步页刷新商店 → 安装 WebDAV 同步插件 → 选中 → 填配置 → 测试连接 → 保存应用 → 双端剪贴板同步；卸载激活插件验证回退本机 server
2. 文件同步（single_file 白名单目前仅文本/图片生效）：需 macOS/Linux/Windows 剪贴板文件路径读写（NSPasteboard / x11 / CF_HDROP），工程量最大
3. 真实多设备端到端验证（desktop 开启 server → 桌面复制文本/图片 → 另一设备同步）

## 已完成

- [x] **同步插件框架（本次）**：
  - client `plugin/` 模块：mod（Lua↔Transport 桥接，配置路径显式传入，修正重装丢配置）/ schema（configSchema 宽松解析：text/secret/button、helpText/placeholder/action，对齐线上真实插件）/ registry（SyncPluginManager：list_sync/配置读写 host.config 同源/安装卸载/make_transport/testConnection）/ index（index.yaml 解析 + clipboard_sync 过滤 + .xipk 下载 sha256 校验）
  - config 分派：`AdapterKind::Plugin`，`make_transport` feature 门控（未启用/构建失败回退 HTTP 轮询并 warn）
  - app 接线：feature `plugin`（tray 依赖它）；desktop 后台命令（索引刷新/下载安装/卸载/测试连接 spawn_blocking）；ApplySettings 写 `config/<id>.yaml`；卸载激活插件时清除选择并重建引擎
  - UI：插件面板页（已安装 + 商店）+ 设置插件适配器（动态表单/单选激活/测试连接状态）
- [x] **同步设置框架**：`client::settings::SyncSettings` + 引擎遵循 + UI 卡片 + 引擎重建 + 下载通知
- [x] **client 直连**：WebDAV、S3（手写 SigV4）、4 个 TCP mock 端到端测试
- [x] **图片同步**：Profile 嵌 base64 data + 引擎/服务端/UI 全链
- [x] Linux 桌面修复、三形态架构 + 发布打包 + CI（详见 git 历史）

## 待办（后续功能点）

- [ ] GUI 手工端到端验证插件同步（见下一步 1）
- [ ] 文件同步（跨平台剪贴板文件路径读写，工作量最大）
- [ ] macOS .app 打包（LSUIElement + 图标）
- [ ] S3 存储后端（按需，大依赖）

## 关键实现细节（本次会话）

- **configSchema 约定**（ximed 定义，libximecore 只存宽松 Value）：字段 `key/label/type/required/placeholder/helpText/action`；type ∈ text|secret|button（未知按 text）；required 缺省视为必填（线上必填字段惯常省略该键）；值统一按字符串读写（host.config 为 String→String YAML）
- **插件配置存储**：`{config_dir}/plugins/config/<id>.yaml`，与插件运行时 `host.config` 同源——设置表单只是该文件的编辑器，插件自身 host.config.set 写的值重启后表单可见；不存 AppSettings（那里只有 plugin_id）
- **插件目录独立**：`{config_dir}/plugins`，不与 XimeChe 的 `~/.config/xime/plugins` 共享（daemon 会扫描重载 emoji 插件，混用 registry 互相干扰）；.xipk 包本身双端通用
- **UI 层与插件层解耦**：ui.rs 只消费纯数据（InstalledPlugin/PluginFieldSpec/StorePlugin），desktop.rs 负责 client::plugin 类型转换——ui.rs 无需感知 feature 门控
- **feature 结构**：app `tray` 依赖 `plugin`（桌面才有插件面板），`--no-default-features` 最小构建两者都不含；client `plugin = [dep:xime-plugin, dep:serde_yaml]`
- **rustup 代理坑（本机开发环境）**：ZCode 沙箱改写 argv[0]，rustup 代理（~/.cargo/bin/cargo→rustup）报 unknown proxy name；用 `~/.rustup/toolchains/stable-*/bin/{cargo,rustc}` 直连 + `RUSTC=` 环境变量绕过
- **GPG 提交**：`git commit -S`；gpg-agent 口令缓存过期后 pinentry 超时需用户重新解锁
