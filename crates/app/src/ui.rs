//! 桌面 UI：iced 窗口（Form 2/3 桌面形态），左右分栏。
//!
//! 布局：
//! ```text
//! ┌──────────┬──────────────────────────┐
//! │ sidebar  │   content（按 panel 切换）  │
//! │  状态    │  状态（含 server 控制区）   │
//! │  历史    │  历史                     │
//! └──────────┴──────────────────────────┘
//! ```
//! 服务器控制区仅 desktop 形态显示（server 在本地可启停）；
//! Form 3 client-only 只读远程服务器状态，无控制区。
//!
//! 线程模型：
//! - 主线程：iced `application` 事件循环（窗口渲染）
//! - 后台线程：tokio runtime（server + client 引擎 + 历史轮询）
//! - 桥接：mpsc channel，后台线程发 [`UiMsg`] 给 UI；UI 经 [`UiCmd`] 通道命令后台

use client::config::ClientConfig;
use client::history::HistoryItem;
use iced::font::Weight;
use iced::widget::{
    Space, TextInput, button, column, container, image, row, scrollable, text, text_input, toggler,
};
use iced::{
    Alignment, Background, Border, Color, Element, Font, Length, Subscription, Task, Theme, border,
};

use crate::settings::{AppSettings, SyncMode};
use client::config::AdapterKind;

/// 面板切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Status,
    History,
    /// 同步：同步方式（插件选择 + 插件配置表单）+ 同步开关 + 插件商店。
    Sync,
}

/// 后台 → UI 的桌面级事件（跨线程 channel 负载）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiMsg {
    /// 同步引擎已启动（携带来源设备）。
    EngineStarted { source: String },
    /// 同步引擎已停止。
    EngineStopped { source: String },
    /// 服务器已监听。
    ServerListening {
        addr: String,
        /// 局域网可直连地址（如 `192.168.1.5:4455`），无可用路由时为 None。
        lan: Option<String>,
    },
    /// 服务器已停止。
    ServerStopped { addr: String },
    /// 剪贴板历史刷新。
    History { items: Vec<HistoryItem> },
    /// client 连接状态上报（引擎 push/拉取结果）。
    SyncStatus { online: bool },
    /// 收到远端剪贴板内容（通知开关开启时上报，携带来源设备）。
    Downloaded { source: Option<String> },
    /// 设置已保存并应用（连接模式/远程服务器已生效）。
    SettingsApplied,
    /// 插件商店索引已加载（仅 clipboard_sync 条目）。
    PluginStoreLoaded {
        updated_at: String,
        entries: Vec<StorePlugin>,
    },
    /// 插件商店索引拉取/解析失败。
    PluginStoreFailed { message: String },
    /// 已安装同步插件列表刷新（启动/安装/卸载后推送）。
    PluginInstalled { plugins: Vec<InstalledPlugin> },
    /// 单个插件安装完成（error 为 None 表示成功）。
    PluginInstallDone { name: String, error: Option<String> },
    /// 插件 testConnection 结果（ok = false 时 message 为失败原因）。
    PluginTestResult { ok: bool, message: String },
    /// 本机 server 连接信息（desktop 后台启动时下发，供 ximed_sync 插件表单自动填充）。
    LocalServerInfo(LocalServerInfo),
    /// 显示窗口（托盘菜单/左键点击触发）。
    ShowWindow,
    /// 隐藏窗口到托盘（托盘菜单触发）。
    HideWindow,
    /// 请求退出（托盘菜单）。
    Quit,
}

/// UI → 后台的桌面级命令（跨线程 channel 负载）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiCmd {
    /// 启动本地 server。
    ServerStart,
    /// 重启本地 server（先停后启）。
    ServerRestart,
    /// 停止本地 server。
    ServerStop,
    /// 打开配置文件所在目录。
    OpenConfigDir,
    /// 应用新设置（同步适配器 + 连接模式 + 远端/插件表单 + 同步设置），持久化并重启 client 引擎。
    ApplySettings(Box<ApplySettingsPayload>),
    /// 拉取插件商店索引（clipboard_sync 条目）。
    PluginRefreshIndex,
    /// 从商店下载并安装同步插件。
    PluginInstall {
        url: String,
        sha256: String,
        name: String,
    },
    /// 卸载已安装的同步插件。
    PluginUninstall { id: String },
    /// 保存插件表单当前值并调用插件 testConnection 契约。
    PluginTestConnection {
        id: String,
        values: Vec<(String, String)>,
    },
}

/// 插件配置字段渲染规格（后台自 configSchema 转换的纯数据，UI 不感知 Lua 插件层）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginFieldSpec {
    pub key: String,
    pub label: String,
    pub kind: PluginFieldKind,
    pub required: bool,
    pub placeholder: String,
    pub help_text: String,
}

/// 字段渲染类型（宽松对应 manifest 的 text/secret/button）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginFieldKind {
    Text,
    Secret,
    Button,
}

/// 已安装的同步插件（含 schema 字段与当前配置值，供设置页动态表单）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub fields: Vec<PluginFieldSpec>,
    /// 当前配置（key → value）。
    pub values: Vec<(String, String)>,
}

/// 插件商店条目（index.yaml 中 pluginType == clipboard_sync 的远端插件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub url: String,
    pub sha256: String,
}

/// 识别为"对接本机 server"的同步插件：装载表单时空缺字段自动填充本地连接配置。
pub const XIMED_SYNC_PLUGIN_ID: &str = "com.kingzcheung.xime.plugin.ximed_sync";

/// 本机 server 连接信息（desktop 后台下发给 UI，供 [`XIMED_SYNC_PLUGIN_ID`] 表单预填）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalServerInfo {
    pub url: String,
    pub username: String,
    pub password: String,
}

/// 设置保存载荷（独立结构避免 UiCmd variant 大小差异过大）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplySettingsPayload {
    pub adapter: client::config::AdapterKind,
    pub mode: SyncMode,
    pub remote_url: String,
    pub remote_username: String,
    pub remote_password: String,
    /// 激活的同步插件 id（adapter = plugin 时生效）。
    pub plugin_id: String,
    /// 插件表单当前值（adapter = plugin 时由后台写入 config/<id>.yaml）。
    pub plugin_values: Vec<(String, String)>,
    pub sync: client::settings::SyncSettings,
}

/// UI 消息（iced Message：channel 事件 + 按钮动作）。
#[derive(Debug, Clone)]
pub enum Message {
    /// 来自后台线程的状态事件。
    Backend(UiMsg),
    /// 窗口已打开（记录 window id，供显示/隐藏控制）。
    WindowOpened(iced::window::Id),
    /// 窗口大小变化（用于历史卡片换行行数估算）。
    WindowResized(iced::Size),
    /// 窗口关闭请求（点红叉 → 隐藏到托盘而非退出）。
    WindowCloseRequested(iced::window::Id),
    /// 切换到某个面板。
    SelectPanel(Panel),
    /// 展开/收起某条剪贴板历史卡片。
    ToggleHistoryItem(u64),
    /// 启动本地 server。
    ServerStart,
    /// 重启本地 server。
    ServerRestart,
    /// 停止本地 server。
    ServerStop,
    /// 打开配置文件所在目录。
    OpenConfigDir,
    /// 退出按钮。
    Quit,
    /// 同步主开关。
    SettingsSyncSwitch(bool),
    /// 推送开关（本地 → 服务器）。
    SettingsSyncPush(bool),
    /// 拉取开关（服务器 → 本地）。
    SettingsSyncPull(bool),
    /// 上传文本开关。
    SettingsSyncUploadText(bool),
    /// 上传图片开关（框架预留）。
    SettingsSyncUploadImage(bool),
    /// 上传单文件开关（框架预留）。
    SettingsSyncUploadSingleFile(bool),
    /// 上传大小上限（MB，文本输入）。
    SettingsSyncMaxFileMb(String),
    /// 收到远端内容时通知开关。
    SettingsSyncNotify(bool),
    /// 选择作为同步方式的插件（选中即激活，保存后生效）。
    SettingsPluginId(String),
    /// 编辑插件配置字段（key 定位字段，一条消息覆盖所有动态字段）。
    SettingsPluginField { key: String, value: String },
    /// 停用插件同步方式，恢复默认（连接本机 server）。
    SyncUseServer,
    /// 测试插件连接（保存表单当前值后调用 testConnection）。
    PluginTestConnection,
    /// 刷新插件商店索引。
    PluginStoreRefresh,
    /// 安装商店插件（按 id 定位条目）。
    PluginStoreInstall(String),
    /// 卸载已安装插件。
    PluginUninstall(String),
    /// 保存并应用设置。
    ApplySettings,
}

/// 窗口状态。
#[derive(Debug, Clone)]
pub struct UiState {
    /// 服务器地址（配置只读）。
    pub server_url: String,
    /// 本机来源设备（配置只读）。
    pub source: String,
    /// 服务器状态文本（动态）。
    pub server_status: String,
    /// 服务器是否已监听（控制按钮状态依据）。
    pub server_running: bool,
    /// 引擎状态文本（动态）。
    pub engine_status: String,
    /// 当前面板。
    pub panel: Panel,
    /// 历史列表（动态）。
    pub history: Vec<HistoryItem>,
    /// 展开的历史条目序号（seq；点击卡片展开/收起，只允许一个展开）。
    pub expanded_history: Option<u64>,
    /// 是否显示服务器控制区（desktop 模式 true，client-only false）。
    pub local_server: bool,
    /// 配置目录路径（打开文件管理器用）。
    pub config_dir: String,
    /// 连接模式（本地 / 远程 server）。
    pub sync_mode: SyncMode,
    /// 同步适配器（server / plugin）。
    pub adapter: client::config::AdapterKind,
    /// 远程服务器地址（设置面板编辑）。
    pub remote_url: String,
    /// 远程服务器用户名（设置面板编辑）。
    pub remote_username: String,
    /// 远程服务器密码（设置面板编辑）。
    pub remote_password: String,
    /// 同步主开关（设置面板编辑）。
    pub sync_switch_on: bool,
    /// 推送开关（设置面板编辑）。
    pub sync_push_on: bool,
    /// 拉取开关（设置面板编辑）。
    pub sync_pull_on: bool,
    /// 上传文本开关（设置面板编辑）。
    pub sync_upload_text: bool,
    /// 上传图片开关（设置面板编辑，框架预留）。
    pub sync_upload_image: bool,
    /// 上传单文件开关（设置面板编辑，框架预留）。
    pub sync_upload_single_file: bool,
    /// 上传大小上限（MB，文本输入，设置面板编辑）。
    pub sync_max_file_mb: String,
    /// 收到远端内容时通知开关（设置面板编辑）。
    pub sync_notify: bool,
    /// 已安装的同步插件（后台推送，含 schema 与当前配置）。
    pub installed_plugins: Vec<InstalledPlugin>,
    /// 插件商店条目（后台拉取的 clipboard_sync 插件）。
    pub store_plugins: Vec<StorePlugin>,
    /// 插件商店状态文本（更新时间/错误/进行中提示）。
    pub store_status: String,
    /// 设置页选中的插件 id（adapter = plugin 时生效）。
    pub plugin_id: String,
    /// 选中插件的 schema 字段（动态表单渲染依据）。
    pub plugin_fields: Vec<PluginFieldSpec>,
    /// 与 plugin_fields 平行的编辑态值。
    pub plugin_values: Vec<String>,
    /// 插件测试连接状态文本。
    pub plugin_test_status: String,
    /// 本机 server 连接信息（desktop 下发；None 为 client-only 形态）。
    pub local_server_info: Option<LocalServerInfo>,
    /// 设置保存状态提示文本。
    pub settings_status: String,
    /// client 是否已连上 server（引擎上报）。
    pub sync_online: bool,
    /// 后台命令发送端。
    cmd_tx: UiCmdSender,
    /// 后台事件发送端（托盘事件也经此注入）。
    tx: UiSender,
    /// 主窗口 id（首次打开时记录，供显示/隐藏控制）。
    window_id: Option<iced::window::Id>,
    /// 主窗口宽度（逻辑像素），用于估算历史卡片内容是否超过 3 行。
    window_width: f32,
    /// 托盘是否已创建（首个 Backend 消息时创建，主线程）。
    tray_started: bool,
}

impl UiState {
    pub fn new(
        cfg: &ClientConfig,
        app_settings: &AppSettings,
        tx: UiSender,
        cmd_tx: UiCmdSender,
        local_server: bool,
        config_dir: String,
    ) -> Self {
        Self {
            server_url: cfg.server_url.clone(),
            source: cfg.source.clone(),
            server_status: "stopped".to_string(),
            server_running: false,
            engine_status: "starting…".to_string(),
            panel: Panel::Status,
            history: Vec::new(),
            expanded_history: None,
            local_server,
            config_dir,
            sync_mode: app_settings.mode,
            adapter: app_settings.adapter,
            remote_url: app_settings.remote_url.clone(),
            remote_username: app_settings.remote_username.clone(),
            remote_password: app_settings.remote_password.clone().unwrap_or_default(),
            sync_switch_on: app_settings.sync.switch_on,
            sync_push_on: app_settings.sync.push_switch_on,
            sync_pull_on: app_settings.sync.pull_switch_on,
            sync_upload_text: app_settings.sync.upload_text,
            sync_upload_image: app_settings.sync.upload_image,
            sync_upload_single_file: app_settings.sync.upload_single_file,
            sync_max_file_mb: (app_settings.sync.max_file_byte / (1024 * 1024)).to_string(),
            sync_notify: app_settings.sync.notify_on_downloaded,
            installed_plugins: Vec::new(),
            store_plugins: Vec::new(),
            store_status: String::new(),
            plugin_id: app_settings.plugin_id.clone(),
            plugin_fields: Vec::new(),
            plugin_values: Vec::new(),
            plugin_test_status: String::new(),
            local_server_info: None,
            settings_status: String::new(),
            sync_online: false,
            cmd_tx,
            tx,
            window_id: None,
            window_width: 1024.0,
            tray_started: false,
        }
    }
}

/// 后台事件发送端（调用方在后台线程 clone 使用）。
pub type UiSender = std::sync::mpsc::Sender<UiMsg>;

/// 后台命令发送端（UI 经此命令后台线程）。
pub type UiCmdSender = std::sync::mpsc::Sender<UiCmd>;

/// 创建后台事件通道（std mpsc，后台线程 try_send 不阻塞）。
pub fn ui_channel() -> (UiSender, std::sync::mpsc::Receiver<UiMsg>) {
    std::sync::mpsc::channel()
}

/// 创建 UI → 后台命令通道。
pub fn ui_cmd_channel() -> (UiCmdSender, std::sync::mpsc::Receiver<UiCmd>) {
    std::sync::mpsc::channel()
}

/// 运行桌面 UI：阻塞直到窗口关闭。
///
/// - `local_server`：desktop 模式为 true（本地 server 可启停），client-only 为 false。
/// - `config_dir`：配置文件所在目录（打开文件管理器用）。
pub fn run_ui(
    cfg: ClientConfig,
    app_settings: AppSettings,
    tx: UiSender,
    cmd_tx: UiCmdSender,
    rx: std::sync::mpsc::Receiver<UiMsg>,
    local_server: bool,
    config_dir: String,
) -> iced::Result {
    let state = UiState::new(&cfg, &app_settings, tx, cmd_tx, local_server, config_dir);
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    // 窗口图标：编译期嵌入 PNG（512×512 RGBA），解码失败视为程序 bug。
    let icon = iced::window::icon::from_file_data(include_bytes!("../assets/icon.png"), None)
        .expect("embedded window icon must decode");
    iced::application(move || (state.clone(), Task::none()), update, view)
        .title("ximed")
        .window(iced::window::Settings {
            icon: Some(icon),
            // Linux 桌面环境（GNOME 等）不读窗口图标，而是按 app_id 匹配
            // ~/.local/share/applications|/usr/share/applications 下的 .desktop 文件
            // 取图标；app_id 必须等于 .desktop 文件名（cargo-packager 打包产物即
            // ximed-desktop.desktop），否则任务栏显示系统默认图标。
            #[cfg(target_os = "linux")]
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "ximed-desktop".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .exit_on_close_request(false)
        .subscription(move |_state| {
            Subscription::batch([backend_events(rx.clone()), window_events()])
        })
        .theme(theme)
        .run()
}

/// 窗口事件订阅：记录窗口 id（打开时）、窗口尺寸、拦截关闭请求（隐藏到托盘）。
fn window_events() -> Subscription<Message> {
    Subscription::batch([
        iced::window::open_events().map(Message::WindowOpened),
        iced::window::resize_events().map(|(_id, size)| Message::WindowResized(size)),
        iced::window::close_requests().map(Message::WindowCloseRequested),
    ])
}

/// 固定主题（状态无关）。
fn theme(_state: &UiState) -> iced::Theme {
    let mut palette = iced::theme::Palette::TOKYO_NIGHT;
    palette.background = BG;
    palette.text = TEXT_MAIN;
    palette.primary = ACCENT;
    palette.success = GREEN;
    palette.danger = RED;
    Theme::custom(String::from("ximed"), palette)
}

fn update(state: &mut UiState, message: Message) -> Task<Message> {
    // 首个后台事件到达时（任意变体）在主线程创建托盘：此时事件循环已运行
    if matches!(&message, Message::Backend(_)) {
        state.ensure_tray();
    }
    match message {
        Message::Backend(UiMsg::EngineStarted { source }) => {
            state.engine_status = format!("engine running (device {source})");
        }
        Message::Backend(UiMsg::EngineStopped { source }) => {
            state.engine_status = format!("engine stopped (device {source})");
        }
        Message::Backend(UiMsg::ServerListening { addr, lan }) => {
            state.server_status = match lan {
                Some(lan) => format!("listening on {addr} · 局域网 {lan}"),
                None => format!("listening on {addr}"),
            };
            state.server_running = true;
        }
        Message::Backend(UiMsg::ServerStopped { addr }) => {
            state.server_status = format!("stopped ({addr})");
            state.server_running = false;
        }
        Message::Backend(UiMsg::History { items }) => {
            state.history = items;
        }
        Message::Backend(UiMsg::SyncStatus { online }) => {
            state.sync_online = online;
        }
        Message::Backend(UiMsg::Downloaded { source }) => {
            let from = source.unwrap_or_else(|| "未知设备".to_string());
            state.settings_status = format!("已从 {from} 同步新内容。");
        }
        Message::Backend(UiMsg::SettingsApplied) => {
            state.settings_status = "已保存并应用。".to_string();
        }
        Message::Backend(UiMsg::PluginStoreLoaded {
            updated_at,
            entries,
        }) => {
            state.store_plugins = entries;
            state.store_status = if updated_at.is_empty() {
                String::new()
            } else {
                format!("索引更新于 {updated_at}")
            };
        }
        Message::Backend(UiMsg::PluginStoreFailed { message }) => {
            state.store_status = format!("索引拉取失败：{message}");
        }
        Message::Backend(UiMsg::PluginInstalled { plugins }) => {
            let selection_lost = state.adapter == AdapterKind::Plugin
                && !state.plugin_id.is_empty()
                && !plugins.iter().any(|p| p.id == state.plugin_id);
            state.installed_plugins = plugins;
            if state.adapter == AdapterKind::Plugin {
                // 当前选中插件被卸载/丢失：清空表单编辑态并回退默认同步方式
                let current = state.plugin_id.clone();
                state.load_plugin_edit_state(&current);
                if selection_lost {
                    state.plugin_id.clear();
                    state.adapter = AdapterKind::Server;
                }
            }
        }
        Message::Backend(UiMsg::PluginInstallDone { name, error }) => match error {
            None => state.store_status = format!("已安装 {name}。"),
            Some(e) => state.store_status = format!("安装 {name} 失败：{e}"),
        },
        Message::Backend(UiMsg::PluginTestResult { ok, message }) => {
            state.plugin_test_status = if ok {
                "连接成功。".to_string()
            } else {
                format!("连接失败：{message}")
            };
        }
        Message::Backend(UiMsg::LocalServerInfo(info)) => {
            state.local_server_info = Some(info);
        }
        Message::Backend(UiMsg::ShowWindow) => {
            return show_window(state);
        }
        Message::Backend(UiMsg::HideWindow) => {
            return hide_window(state);
        }
        Message::Backend(UiMsg::Quit) => {
            return iced::exit();
        }
        Message::WindowOpened(id) => {
            state.window_id = Some(id);
        }
        Message::WindowResized(size) => {
            state.window_width = size.width;
        }
        Message::WindowCloseRequested(id) => {
            state.window_id = Some(id);
            // 关闭按钮：隐藏到托盘而非退出
            return hide_window(state);
        }
        Message::SelectPanel(panel) => {
            state.panel = panel;
        }
        Message::ToggleHistoryItem(seq) => {
            state.expanded_history = if state.expanded_history == Some(seq) {
                None
            } else {
                Some(seq)
            };
        }
        Message::ServerStart => {
            state.server_status = "starting…".to_string();
            let _ = state.cmd_tx.send(UiCmd::ServerStart);
        }
        Message::ServerRestart => {
            state.server_status = "restarting…".to_string();
            let _ = state.cmd_tx.send(UiCmd::ServerRestart);
        }
        Message::ServerStop => {
            state.server_status = "stopping…".to_string();
            let _ = state.cmd_tx.send(UiCmd::ServerStop);
        }
        Message::OpenConfigDir => {
            let _ = state.cmd_tx.send(UiCmd::OpenConfigDir);
        }
        Message::SettingsSyncSwitch(on) => {
            state.sync_switch_on = on;
            state.settings_status = String::new();
        }
        Message::SettingsSyncPush(on) => {
            state.sync_push_on = on;
            state.settings_status = String::new();
        }
        Message::SettingsSyncPull(on) => {
            state.sync_pull_on = on;
            state.settings_status = String::new();
        }
        Message::SettingsSyncUploadText(on) => {
            state.sync_upload_text = on;
            state.settings_status = String::new();
        }
        Message::SettingsSyncUploadImage(on) => {
            state.sync_upload_image = on;
            state.settings_status = String::new();
        }
        Message::SettingsSyncUploadSingleFile(on) => {
            state.sync_upload_single_file = on;
            state.settings_status = String::new();
        }
        Message::SettingsSyncMaxFileMb(v) => {
            // 仅允许数字，非法输入忽略
            if v.is_empty() || v.parse::<u64>().is_ok() {
                state.sync_max_file_mb = v;
                state.settings_status = String::new();
            }
        }
        Message::SettingsSyncNotify(on) => {
            state.sync_notify = on;
            state.settings_status = String::new();
        }
        Message::SettingsPluginId(id) => {
            // 选中即激活（保存后生效）；选中插件缺失时回退第一个已安装插件
            state.plugin_id = id;
            state.adapter = AdapterKind::Plugin;
            state.plugin_test_status = String::new();
            state.settings_status = String::new();
            if !state
                .installed_plugins
                .iter()
                .any(|p| p.id == state.plugin_id)
            {
                state.plugin_id = state
                    .installed_plugins
                    .first()
                    .map(|p| p.id.clone())
                    .unwrap_or_default();
            }
            let current = state.plugin_id.clone();
            state.load_plugin_edit_state(&current);
        }
        Message::SyncUseServer => {
            // 停用插件方式，恢复默认（连接本机 server）；保存后生效
            state.adapter = AdapterKind::Server;
            state.plugin_test_status = String::new();
            state.settings_status =
                "将恢复默认（连接本机 server），点击「保存并应用」生效。".to_string();
        }
        Message::SettingsPluginField { key, value } => {
            if let Some(idx) = state.plugin_fields.iter().position(|f| f.key == key)
                && let Some(slot) = state.plugin_values.get_mut(idx)
            {
                *slot = value;
            }
            state.plugin_test_status = String::new();
        }
        Message::PluginTestConnection => {
            if state.plugin_id.is_empty() {
                state.plugin_test_status = "请先选择插件。".to_string();
            } else {
                state.plugin_test_status = "测试中…".to_string();
                let cmd = UiCmd::PluginTestConnection {
                    id: state.plugin_id.clone(),
                    values: state.plugin_form_values(),
                };
                let _ = state.cmd_tx.send(cmd);
            }
        }
        Message::PluginStoreRefresh => {
            state.store_status = "正在刷新索引…".to_string();
            let _ = state.cmd_tx.send(UiCmd::PluginRefreshIndex);
        }
        Message::PluginStoreInstall(id) => match state.store_plugins.iter().find(|p| p.id == id) {
            Some(entry) => {
                state.store_status = format!("正在下载 {}…", entry.name);
                let _ = state.cmd_tx.send(UiCmd::PluginInstall {
                    url: entry.url.clone(),
                    sha256: entry.sha256.clone(),
                    name: entry.name.clone(),
                });
            }
            None => state.store_status = "插件不在当前索引中，请刷新。".to_string(),
        },
        Message::PluginUninstall(id) => {
            let _ = state.cmd_tx.send(UiCmd::PluginUninstall { id });
        }
        Message::ApplySettings => {
            let cmd = UiCmd::ApplySettings(Box::new(ApplySettingsPayload {
                adapter: state.adapter,
                mode: state.sync_mode,
                remote_url: state.remote_url.clone(),
                remote_username: state.remote_username.clone(),
                remote_password: state.remote_password.clone(),
                plugin_id: state.plugin_id.clone(),
                plugin_values: if state.adapter == AdapterKind::Plugin {
                    state.plugin_form_values()
                } else {
                    Vec::new()
                },
                sync: state.sync_settings(),
            }));
            if state.cmd_tx.send(cmd).is_ok() {
                state.settings_status = "正在保存并应用…".to_string();
            } else {
                state.settings_status = "保存失败：后台通道不可用。".to_string();
            }
        }
        Message::Quit => {
            return iced::exit();
        }
    }
    Task::none()
}

impl UiState {
    /// 把设置面板编辑态组装成 [`client::settings::SyncSettings`]。
    /// 大小上限：MB 文本 → 字节（非法输入回退默认 10MB）。
    fn sync_settings(&self) -> client::settings::SyncSettings {
        let max_file_byte = self
            .sync_max_file_mb
            .parse::<u64>()
            .ok()
            .map(|mb| mb.saturating_mul(1024 * 1024))
            .unwrap_or_else(|| 10 * 1024 * 1024);
        client::settings::SyncSettings {
            switch_on: self.sync_switch_on,
            push_switch_on: self.sync_push_on,
            pull_switch_on: self.sync_pull_on,
            upload_text: self.sync_upload_text,
            upload_image: self.sync_upload_image,
            upload_single_file: self.sync_upload_single_file,
            max_file_byte,
            notify_on_downloaded: self.sync_notify,
        }
    }

    /// 插件根目录（提示安装位置用）。
    pub fn plugins_dir(&self) -> String {
        std::path::Path::new(&self.config_dir)
            .join("plugins")
            .display()
            .to_string()
    }

    /// 载入插件的表单编辑态：schema 字段 + 已保存配置值（缺失键为空串）+ 本地预填。
    fn load_plugin_edit_state(&mut self, id: &str) {
        match self.installed_plugins.iter().find(|p| p.id == id) {
            Some(p) => {
                self.plugin_fields = p.fields.clone();
                self.plugin_values = p
                    .fields
                    .iter()
                    .map(|f| {
                        p.values
                            .iter()
                            .find(|(k, _)| k == &f.key)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default()
                    })
                    .collect();
                self.prefill_local_server_config(id);
            }
            None => {
                self.plugin_fields.clear();
                self.plugin_values.clear();
            }
        }
    }

    /// [`XIMED_SYNC_PLUGIN_ID`]（对接 ximed server 的同步插件）：本机 server 信息
    /// 可用时，把空缺的 serverUrl/username/password 自动填充为本地连接配置。
    /// 已有值（用户保存过或插件自写）不覆盖。
    fn prefill_local_server_config(&mut self, id: &str) {
        if id != XIMED_SYNC_PLUGIN_ID {
            return;
        }
        let Some(info) = &self.local_server_info else {
            return;
        };
        for (idx, field) in self.plugin_fields.iter().enumerate() {
            let fill = match field.key.as_str() {
                "serverUrl" => Some(&info.url),
                "username" => Some(&info.username),
                "password" => Some(&info.password),
                _ => None,
            };
            if let (Some(v), Some(slot)) = (fill, self.plugin_values.get_mut(idx))
                && slot.is_empty()
            {
                *slot = v.clone();
            }
        }
    }

    /// 当前表单编辑态 → (key, value) 列表（测试连接/保存用）。
    fn plugin_form_values(&self) -> Vec<(String, String)> {
        self.plugin_fields
            .iter()
            .zip(self.plugin_values.iter())
            .filter(|(f, _)| f.kind != PluginFieldKind::Button)
            .map(|(f, v)| (f.key.clone(), v.clone()))
            .collect()
    }

    /// 创建托盘（仅一次）。必须在主线程、事件循环已运行时调用。
    fn ensure_tray(&mut self) {
        if self.tray_started {
            return;
        }
        self.tray_started = true;
        #[cfg(target_os = "macos")]
        crate::macos::set_dock_icon();
        if let Err(e) = crate::tray::setup(self.tx.clone()) {
            eprintln!("[ximed] tray setup failed: {e}");
            tracing::warn!("tray setup failed: {e}");
        }
    }
}

/// 显示主窗口（从托盘恢复）。
fn show_window(state: &UiState) -> Task<Message> {
    match state.window_id {
        Some(id) => iced::window::set_mode(id, iced::window::Mode::Windowed),
        None => Task::none(),
    }
}

/// 隐藏主窗口到托盘（不退出进程）。
fn hide_window(state: &UiState) -> Task<Message> {
    match state.window_id {
        Some(id) => iced::window::set_mode(id, iced::window::Mode::Hidden),
        None => Task::none(),
    }
}

// ---------------------------------------------------------------------------
// 现代化样式：Tailwind 风格（zinc 深色 + indigo 主色，圆角卡片、胶囊、柔和分层）
// ---------------------------------------------------------------------------

/// 主题颜色常量（zinc 色阶 + indigo 强调色，贴近 Tailwind dark UI）。
const BG: Color = Color::from_rgb8(0x0a, 0x0a, 0x0c); // 窗口底色 zinc-950
const SURFACE: Color = Color::from_rgb8(0x15, 0x15, 0x18); // 卡片/侧栏 zinc-900
const SURFACE_HOVER: Color = Color::from_rgb8(0x22, 0x22, 0x27); // 悬停 zinc-800
const BORDER: Color = Color::from_rgb8(0x2a, 0x2a, 0x30); // 边框 zinc-800
const BORDER_STRONG: Color = Color::from_rgb8(0x3f, 0x3f, 0x46); // 边框强调 zinc-700
const TEXT_MAIN: Color = Color::from_rgb8(0xf4, 0xf4, 0xf5); // 主文本 zinc-100
const TEXT_MUTED: Color = Color::from_rgb8(0xa1, 0xa1, 0xaa); // 次级文本 zinc-400
const TEXT_FAINT: Color = Color::from_rgb8(0x71, 0x71, 0x7a); // 弱化文本 zinc-500
const ACCENT: Color = Color::from_rgb8(0x81, 0x8c, 0xf8); // 主色 indigo-400（深底可读）
const ACCENT_STRONG: Color = Color::from_rgb8(0x63, 0x66, 0xf1); // 主色填充 indigo-500
const ACCENT_DIM: Color = Color::from_rgba8(0x63, 0x66, 0xf1, 0.18); // 主色浅底（active 导航）
const GREEN: Color = Color::from_rgb8(0x34, 0xd3, 0x99); // 成功 emerald-400
const GREEN_DIM: Color = Color::from_rgba8(0x34, 0xd3, 0x99, 0.15);
const RED: Color = Color::from_rgb8(0xfb, 0x71, 0x85); // 危险 rose-400
const RED_DIM: Color = Color::from_rgba8(0xfb, 0x71, 0x85, 0.15);

/// 圆角（Tailwind rounded-lg/xl）。
const RADIUS_LG: f32 = 12.0;
const RADIUS_MD: f32 = 10.0;
const RADIUS_SM: f32 = 8.0;

/// 标题字体（semibold）。
const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..Font::DEFAULT
};

/// 卡片容器样式：圆角 + 边框 + 柔和底色。
fn card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT_MAIN),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::radius(RADIUS_LG),
        },
        ..container::Style::default()
    }
}

/// 侧栏样式：无圆角的整块面板，与窗口底色分层。
fn sidebar_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT_MAIN),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(0.0),
        },
        ..container::Style::default()
    }
}

/// 导航按钮样式：active 为主色浅底 + 主色文字；悬停为浅色底。
fn nav_style(_theme: &Theme, status: button::Status, active: bool) -> button::Style {
    let base = button::Style {
        background: None,
        text_color: TEXT_MUTED,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(RADIUS_MD),
        },
        ..button::Style::default()
    };
    if active {
        button::Style {
            background: Some(Background::Color(ACCENT_DIM)),
            text_color: ACCENT,
            ..base
        }
    } else {
        let bg = match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(Background::Color(SURFACE_HOVER))
            }
            _ => None,
        };
        button::Style {
            background: bg,
            ..base
        }
    }
}

/// 主要操作按钮（开启/保存）：indigo 填充 + 白色文字。
fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered => Color::from_rgb8(0x81, 0x8c, 0xf8),
        button::Status::Pressed => Color::from_rgb8(0x4f, 0x52, 0xe4),
        _ => ACCENT_STRONG,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: Color::WHITE,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(RADIUS_MD),
        },
        ..button::Style::default()
    }
}

/// 停止按钮：rose 填充 + 白色文字。
fn stop_button(_theme: &Theme, status: button::Status) -> button::Style {
    let bg = if status == button::Status::Hovered {
        Color::from_rgb8(0xff, 0x8f, 0xa3)
    } else {
        RED
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: Color::WHITE,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(RADIUS_MD),
        },
        ..button::Style::default()
    }
}

/// 次要操作按钮：透明底 + 边框，悬停浅色底。
fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: if hovered {
            Some(Background::Color(SURFACE_HOVER))
        } else {
            None
        },
        text_color: TEXT_MAIN,
        border: Border {
            color: BORDER_STRONG,
            width: 1.0,
            radius: border::radius(RADIUS_MD),
        },
        ..button::Style::default()
    }
}

/// 危险按钮（退出）：透明底 + rose 文字 + 悬停浅红底。
fn danger_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: if hovered {
            Some(Background::Color(RED_DIM))
        } else {
            None
        },
        text_color: RED,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(RADIUS_MD),
        },
        ..button::Style::default()
    }
}

/// 危险描边按钮（小尺寸动作，如插件卸载）。
fn danger_outline_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: if hovered {
            Some(Background::Color(RED_DIM))
        } else {
            None
        },
        text_color: RED,
        border: Border {
            color: if hovered { RED } else { BORDER_STRONG },
            width: 1.0,
            radius: border::radius(RADIUS_SM),
        },
        ..button::Style::default()
    }
}

/// 文本输入框样式：深色底 + 边框，聚焦时主色描边。
fn input_style(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let border_color = match status {
        text_input::Status::Focused { .. } => ACCENT,
        text_input::Status::Hovered => BORDER_STRONG,
        _ => BORDER,
    };
    text_input::Style {
        background: Background::Color(SURFACE),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: border::radius(RADIUS_MD),
        },
        icon: TEXT_MUTED,
        placeholder: TEXT_FAINT,
        value: TEXT_MAIN,
        selection: Color::from_rgba8(0x63, 0x66, 0xf1, 0.35),
    }
}

// ---------------------------------------------------------------------------
// 视图
// ---------------------------------------------------------------------------

fn view(state: &UiState) -> Element<'_, Message> {
    // 左分栏：品牌 + 面板导航
    let mut nav = column![
        brand(),
        Space::new().height(20),
        text("菜单").size(11).color(TEXT_FAINT),
        Space::new().height(4),
        nav_button("状态", Panel::Status, state.panel),
        nav_button("剪贴板历史", Panel::History, state.panel),
    ]
    .spacing(2)
    .padding([20, 14]);

    // 同步面板仅 desktop 形态显示（client-only 无设置能力，同步全默认）
    if state.local_server {
        nav = nav.push(nav_button("同步", Panel::Sync, state.panel));
    }

    // 侧栏底部：退出
    nav = nav.push(Space::new().height(Length::Fill));
    nav = nav.push(
        button(text("退出").color(RED))
            .on_press(Message::Quit)
            .width(Length::Fill)
            .style(danger_button),
    );

    let sidebar = container(nav)
        .width(200)
        .height(Length::Fill)
        .style(sidebar_style);

    // 右分栏：按面板渲染内容
    let content = match state.panel {
        Panel::Status => status_view(state),
        Panel::History => history_view(state),
        Panel::Sync => sync_view(state),
    };
    let content = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20);

    row![sidebar, content]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 品牌区：应用图标 + 名称 + 副标题。
fn brand() -> Element<'static, Message> {
    // 编译期嵌入应用图标（1024×1024 RGBA），与窗口图标同源。
    let logo = image(image::Handle::from_bytes(
        include_bytes!("../assets/icon.png").as_slice(),
    ))
    .width(32)
    .height(32);
    row![
        logo,
        column![
            text("ximed").size(15).font(SEMIBOLD).color(TEXT_MAIN),
            text("剪贴板同步").size(11).color(TEXT_MUTED),
        ]
        .spacing(1),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
}

fn nav_button(label: &str, panel: Panel, active: Panel) -> Element<'_, Message> {
    let is_active = panel == active;
    button(
        row![
            dot(is_active),
            text(label)
                .size(13)
                .font(if is_active { SEMIBOLD } else { Font::DEFAULT })
                .color(if is_active { ACCENT } else { TEXT_MUTED }),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .on_press(Message::SelectPanel(panel))
    .width(Length::Fill)
    .padding([8, 10])
    .style(move |theme, status| nav_style(theme, status, is_active))
    .into()
}

/// 导航项左侧的小圆点指示器（active 时主色实心，否则空圆环）。
fn dot(active: bool) -> Element<'static, Message> {
    let color = if active { ACCENT } else { BORDER_STRONG };
    container(text("").width(7).height(7))
        .width(7)
        .height(7)
        .style(move |_| container::Style {
            background: if active {
                Some(Background::Color(ACCENT))
            } else {
                None
            },
            border: Border {
                color,
                width: 1.0,
                radius: border::radius(999.0),
            },
            ..container::Style::default()
        })
        .into()
}

fn status_view(state: &UiState) -> Element<'_, Message> {
    let server_up = state.server_running;
    let engine_up = state.engine_status.starts_with("engine running");
    let server_value = if server_up {
        format!("{} @ {}", state.server_status, state.server_url)
    } else {
        state.server_status.clone()
    };

    let info_card = container(
        column![
            card_title("服务与同步引擎"),
            stat_row("服务器", server_value, server_up, server_hint(state)),
            stat_row("设备", state.source.clone(), true, None),
            stat_row("同步引擎", state.engine_status.clone(), engine_up, None),
        ]
        .spacing(10)
        .padding(16),
    )
    .width(Length::Fill)
    .style(card);

    let mut content = column![
        panel_header("状态", "服务与同步引擎运行情况"),
        Space::new().height(8),
        info_card,
    ]
    .spacing(10);

    // 服务器控制区：仅本地 server（desktop 形态 + 本地模式）显示；
    // 远程模式 client 连远程 server，本地 server 控制无意义。
    if state.local_server && state.sync_mode == SyncMode::Local {
        content = content.push(server_controls(state));
    }
    content = content.push(text(&state.config_dir).size(11).color(TEXT_FAINT));
    scrollable(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 面板页头：标题 + 副标题。
fn panel_header<'a>(title: &'a str, subtitle: &'a str) -> Element<'a, Message> {
    column![
        text(title).size(20).font(SEMIBOLD).color(TEXT_MAIN),
        text(subtitle).size(12).color(TEXT_MUTED),
    ]
    .spacing(2)
    .into()
}

/// 卡片标题。
fn card_title<'a>(label: &'a str) -> Element<'a, Message> {
    text(label).size(14).font(SEMIBOLD).color(TEXT_MAIN).into()
}

/// 服务器状态行的括号提示（按 SyncMode）：
///
/// - 本地模式：server 未运行 → 提示点击「开启」。
/// - 远程模式：连不上 → 提示远程不可用；已连接 → 绿色标注。
fn server_hint(state: &UiState) -> Option<(String, Color)> {
    match state.sync_mode {
        SyncMode::Local => {
            if state.server_running {
                None
            } else {
                Some(("（未启动，请点击「开启」）".to_string(), RED))
            }
        }
        SyncMode::Remote => {
            if state.sync_online {
                Some(("（已连接）".to_string(), GREEN))
            } else {
                Some(("（远程服务器不可用，请检查设置）".to_string(), RED))
            }
        }
    }
}

/// 本地服务器控制卡片：开启 / 重启 / 关闭 + 打开配置文件目录。
fn server_controls(state: &UiState) -> Element<'static, Message> {
    let running = state.server_running;
    let title = if running {
        "服务器运行中"
    } else {
        "服务器已停止"
    };

    let status_badge = capsule(if running { "运行中" } else { "已停止" }, running);

    let mut btns = row![].spacing(8).align_y(Alignment::Center);
    if running {
        btns = btns.push(
            button("重启")
                .on_press(Message::ServerRestart)
                .padding([8, 14])
                .style(secondary_button),
        );
        btns = btns.push(
            button("关闭")
                .on_press(Message::ServerStop)
                .padding([8, 14])
                .style(stop_button),
        );
    } else {
        btns = btns.push(
            button("开启")
                .on_press(Message::ServerStart)
                .padding([8, 18])
                .style(primary_button),
        );
    }

    container(
        column![
            row![
                status_badge,
                text(title).size(15).font(SEMIBOLD).color(TEXT_MAIN)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            text("本地服务器默认不随应用自动启动，请手动开启。")
                .size(12)
                .color(TEXT_MUTED),
            btns,
            button("打开配置文件所在目录")
                .on_press(Message::OpenConfigDir)
                .padding([7, 12])
                .style(secondary_button),
        ]
        .spacing(10)
        .padding(16),
    )
    .width(Length::Fill)
    .style(card)
    .into()
}

/// 状态胶囊：圆角标签，ok 为绿色、否则红色。
fn capsule<'a>(label: &'a str, ok: bool) -> Element<'a, Message> {
    let (fg, bg) = if ok {
        (GREEN, GREEN_DIM)
    } else {
        (RED, RED_DIM)
    };
    container(
        row![
            container(text("").width(6).height(6))
                .width(6)
                .height(6)
                .style(move |_| container::Style {
                    background: Some(Background::Color(fg)),
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: border::radius(999.0),
                    },
                    ..container::Style::default()
                }),
            text(label).size(11).color(fg),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    )
    .padding([3, 10])
    .style(move |_| container::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(999.0),
        },
        ..container::Style::default()
    })
    .into()
}

/// 状态行：彩色圆点 + 标签 + 值 + 可选的括号提示（如「未启动，请点击开启」）。
fn stat_row(
    label: &'static str,
    value: String,
    ok: bool,
    hint: Option<(String, Color)>,
) -> Element<'static, Message> {
    let mut content = row![
        stat_dot(ok),
        text(label)
            .size(13)
            .color(TEXT_MUTED)
            .width(Length::Fixed(72.0)),
        text(value).size(13).color(TEXT_MAIN),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some((hint, color)) = hint {
        content = content.push(text(hint).size(12).color(color));
    }
    content.into()
}

/// 绿/红状态圆点。
fn stat_dot(ok: bool) -> Element<'static, Message> {
    let color = if ok { GREEN } else { RED };
    container(text("").width(10).height(10))
        .width(10)
        .height(10)
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(5.0),
            },
            ..container::Style::default()
        })
        .into()
}

/// 历史卡片内容区收起时的高度（3 行文本 ≈ 3 × 20px）。
const HISTORY_CONTENT_HEIGHT: f32 = 60.0;
/// 历史卡片展开时内容区的最大高度（超出滚动）。
const HISTORY_EXPANDED_HEIGHT: f32 = 160.0;

fn history_view(state: &UiState) -> Element<'_, Message> {
    let mut col = column![
        panel_header("剪贴板历史", "最近同步的剪贴板内容，点击卡片展开全文"),
        Space::new().height(8),
    ]
    .spacing(4);

    if state.history.is_empty() {
        col = col.push(
            container(
                column![
                    text("暂无历史记录")
                        .size(14)
                        .font(SEMIBOLD)
                        .color(TEXT_MUTED),
                    text("复制或同步内容后将在此显示")
                        .size(12)
                        .color(TEXT_FAINT),
                ]
                .spacing(4)
                .align_x(Alignment::Center),
            )
            .width(Length::Fill)
            .padding(24)
            .style(card),
        );
    } else {
        for item in &state.history {
            let expanded = state.expanded_history == Some(item.seq);
            col = col.push(history_card(
                item,
                expanded,
                state.window_width,
                &state.source,
            ));
        }
    }

    scrollable(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(history_scroll_style)
        .into()
}

/// 历史列表滚动条样式：窄滚动条 + 半透明圆角滑块，贴合深色主题。
fn history_scroll_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, status);
    let rail = scrollable::Rail {
        background: None,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(999.0),
        },
        scroller: scrollable::Scroller {
            background: Background::Color(Color {
                a: 0.35,
                ..ACCENT_DIM
            }),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(999.0),
            },
        },
    };
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style
}

/// 单条历史卡片：内容为主、最多 3 行，底部以 tag 展示元信息；点击展开全文。
fn history_card<'a>(
    item: &'a HistoryItem,
    expanded: bool,
    window_width: f32,
    local_source: &'a str,
) -> Element<'a, Message> {
    // 图片类型无文本预览，用占位文字展示（历史库不存像素数据）
    let preview: &str = if item.kind == "image" {
        "[图片]"
    } else {
        &item.text
    };
    // 仅当内容超过 3 行时才允许展开：按内容区宽度估算换行行数。
    let has_more = history_has_more(preview, window_width);

    // 内容区：收起时固定高度 + 裁剪（最多 3 行）；展开时全文（限高滚动，隐藏内部滚动条避免浮在文字上）
    let content = if expanded {
        Element::from(
            container(
                scrollable(text(preview).size(13).color(TEXT_MAIN))
                    .height(Length::Fixed(HISTORY_EXPANDED_HEIGHT))
                    .direction(scrollable::Direction::Vertical(
                        scrollable::Scrollbar::hidden(),
                    )),
            )
            .width(Length::Fill)
            .clip(true),
        )
    } else {
        Element::from(
            container(text(preview).size(13).color(TEXT_MAIN))
                .width(Length::Fill)
                .height(Length::Fixed(HISTORY_CONTENT_HEIGHT))
                .clip(true),
        )
    };

    // 底部 tag 行：类型 / 来源 / 序号 / 时间
    // 来源区分：等于本机设备名 → 本机记录；否则为远程推送（强调色高亮 + 前缀）。
    let is_local = item.source.as_deref() == Some(local_source);
    let source_label = if is_local {
        "本机".to_string()
    } else {
        format!("远程 · {}", item.source.as_deref().unwrap_or("unknown"))
    };
    let source_tag = if is_local {
        history_tag(source_label)
    } else {
        history_tag_remote(source_label)
    };
    let time = format_time(item.created_at);
    let tags = row![
        history_tag(item.kind.clone()),
        source_tag,
        history_tag(format!("#{}", item.seq)),
        history_tag(time),
    ]
    .spacing(6);

    // 展开/收起提示仅在有更多内容时显示；无更多内容时整卡不可点击。
    let mut footer = iced::widget::Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(tags);
    if has_more {
        footer = footer.push(
            text(if expanded { "收起 ▲" } else { "展开 ▼" })
                .size(11)
                .color(ACCENT),
        );
    }

    let card = column![content, footer]
        .spacing(8)
        .align_x(Alignment::Start);

    let mut btn = button(card)
        .width(Length::Fill)
        .height(Length::Shrink)
        .padding(10)
        .style(history_card_style);
    if has_more {
        btn = btn.on_press(Message::ToggleHistoryItem(item.seq));
    }
    btn.into()
}

/// 估算文本在历史卡片内容区中会占据的行数（内容区宽度 = 窗口宽度 - 固定开销）。
fn history_estimated_lines(text: &str, window_width: f32) -> usize {
    // 布局开销：sidebar(160) + row padding(12×2) + 内容卡 padding(16×2) + 卡片按钮 padding(10×2)
    // + 滚动条余量；13px 字号下平均字符宽约 7px（中英文混合取保守值）。
    const LAYOUT_OVERHEAD: f32 = 160.0 + 24.0 + 32.0 + 20.0 + 16.0;
    const AVG_CHAR_WIDTH: f32 = 7.0;
    let content_width = (window_width - LAYOUT_OVERHEAD).max(120.0);
    let chars_per_line = (content_width / AVG_CHAR_WIDTH).floor() as usize;

    let mut lines = 0usize;
    for seg in text.split('\n') {
        // 每段按字符数估算换行后的行数；空段也算 1 行。
        let chars = seg.chars().count();
        lines += if chars == 0 {
            1
        } else {
            chars.div_ceil(chars_per_line.max(1))
        };
    }
    lines
}

/// 内容是否超过 3 行（决定是否提供展开）。
fn history_has_more(text: &str, window_width: f32) -> bool {
    history_estimated_lines(text, window_width) > 3
}

/// 历史卡片按钮样式：卡片底色 + 边框，悬停时高亮。
fn history_card_style(_theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: TEXT_MAIN,
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::radius(RADIUS_LG),
        },
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered) {
        style.background = Some(Background::Color(SURFACE_HOVER));
        style.border = Border {
            color: BORDER_STRONG,
            width: 1.0,
            radius: border::radius(RADIUS_LG),
        };
    }
    style
}

/// 胶囊 tag：元信息小标签。
fn history_tag(label: String) -> Element<'static, Message> {
    container(text(label).size(11).color(TEXT_MUTED))
        .padding([3, 8])
        .style(|_theme| container::Style {
            background: Some(Background::Color(SURFACE_HOVER)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(999.0),
            },
            ..container::Style::default()
        })
        .into()
}

/// 远程来源 tag：强调色文字 + 浅色强调底，与"本机"记录做视觉区分。
fn history_tag_remote(label: String) -> Element<'static, Message> {
    container(text(label).size(11).color(ACCENT))
        .padding([3, 8])
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color { a: 0.12, ..ACCENT })),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(999.0),
            },
            ..container::Style::default()
        })
        .into()
}

/// 相对时间：Unix 秒 → "刚刚 / N 分钟前 / N 小时前 / N 天前"。
fn format_time(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = now - unix;
    if diff < 60 {
        "刚刚".to_string()
    } else if diff < 3600 {
        format!("{} 分钟前", diff / 60)
    } else if diff < 86400 {
        format!("{} 小时前", diff / 3600)
    } else {
        format!("{} 天前", diff / 86400)
    }
}

/// 同步面板：同步方式（插件选择 + 插件配置表单）+ 同步开关 + 插件商店。
///
/// 同步配置完全由插件承载（getSettingsSchema 动态渲染表单，新增插件零硬编码）；
/// 默认/远端路径走本地或远端 ximed server（daemon 配置能力），UI 仅暴露
/// 「恢复默认（连接本机 server）」一键回退。
fn sync_view(state: &UiState) -> Element<'_, Message> {
    let target_card = container(sync_target_body(state))
        .width(Length::Fill)
        .padding(16)
        .style(card);
    let switch_card = container(sync_card_body(state))
        .width(Length::Fill)
        .padding(16)
        .style(card);
    let store_card = container(store_body(state))
        .width(Length::Fill)
        .padding(16)
        .style(card);

    let body = column![
        panel_header("同步", "同步方式（插件）、开关与插件商店"),
        Space::new().height(8),
        target_card,
        Space::new().height(4),
        switch_card,
        Space::new().height(4),
        store_card,
    ]
    .spacing(6);

    container(scrollable(body).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 同步方式卡片：当前生效方式 + 插件选择（选中即激活，含卸载）+ 插件配置动态表单。
fn sync_target_body(state: &UiState) -> Element<'_, Message> {
    let mut body = column![card_title("同步方式")].spacing(8);

    // 当前生效方式
    let active_line = match state.adapter {
        AdapterKind::Plugin => match state
            .installed_plugins
            .iter()
            .find(|p| p.id == state.plugin_id)
        {
            Some(p) => format!("当前生效：插件「{}」", p.name),
            None => "当前生效：插件（未选择，保存后按 HTTP 兜底）".to_string(),
        },
        _ => "当前生效：本机 server（默认）".to_string(),
    };
    body = body.push(text(active_line).size(12).color(TEXT_MUTED));

    if state.installed_plugins.is_empty() {
        body = body.push(
            column![
                text("尚未安装同步插件。").size(12).color(TEXT_MUTED),
                text(format!(
                    "可在下方商店安装；插件目录：{}",
                    state.plugins_dir()
                ))
                .size(11)
                .color(TEXT_FAINT),
            ]
            .spacing(2),
        );
    }
    // 插件选择列表（单选激活，同 manifest activation: single 语义）
    for plugin in &state.installed_plugins {
        let selected = state.plugin_id == plugin.id;
        let is_target = state.adapter == AdapterKind::Plugin && selected;
        body = body.push(
            row![
                button(
                    row![
                        text(if is_target { "●" } else { "○" })
                            .size(12)
                            .color(if selected { ACCENT } else { TEXT_FAINT }),
                        column![
                            text(format!("{} ({})", plugin.name, plugin.version))
                                .size(13)
                                .color(if selected { TEXT_MAIN } else { TEXT_MUTED }),
                            text(&plugin.id).size(11).color(TEXT_FAINT),
                        ]
                        .spacing(1),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .on_press(Message::SettingsPluginId(plugin.id.clone()))
                .padding([8, 10])
                .width(Length::Fill)
                .style(move |_theme, status| {
                    let hovered =
                        matches!(status, button::Status::Hovered | button::Status::Pressed);
                    button::Style {
                        background: Some(Background::Color(if selected {
                            ACCENT_DIM
                        } else if hovered {
                            SURFACE_HOVER
                        } else {
                            SURFACE
                        })),
                        text_color: TEXT_MAIN,
                        border: Border {
                            color: if selected { ACCENT } else { BORDER },
                            width: 1.0,
                            radius: border::radius(RADIUS_MD),
                        },
                        ..button::Style::default()
                    }
                }),
                button(text("卸载").size(12).color(RED))
                    .on_press(Message::PluginUninstall(plugin.id.clone()))
                    .padding([6, 12])
                    .style(danger_outline_button),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }

    // 动态表单（manifest configSchema 驱动）：text/secret → 文本输入（secret 掩码），
    // button → 动作按钮（当前支持 testConnection 契约）；保存写入 config/<id>.yaml，
    // 与插件运行时 host.config 同源，无需为每个插件手写表单。
    for (idx, field) in state.plugin_fields.iter().enumerate() {
        let value = state.plugin_values.get(idx).cloned().unwrap_or_default();
        let label = if field.required {
            format!("{} *", field.label)
        } else {
            field.label.clone()
        };
        body = body.push(Space::new().height(2));
        match field.kind {
            PluginFieldKind::Text | PluginFieldKind::Secret => {
                // label move 进 text（借用会延长到返回值，本地 String 不行）
                body = body.push(text(label).size(12).color(TEXT_MUTED));
                let mut input = TextInput::new(&field.placeholder, &value)
                    .padding(10)
                    .style(input_style);
                input = if field.kind == PluginFieldKind::Secret {
                    input.secure(true)
                } else {
                    input
                };
                body = body.push(input.on_input(move |v| Message::SettingsPluginField {
                    key: field.key.clone(),
                    value: v,
                }));
            }
            PluginFieldKind::Button => {
                body = body.push(
                    button(text(&field.label).size(13))
                        .on_press(Message::PluginTestConnection)
                        .padding([8, 14])
                        .style(primary_button),
                );
            }
        }
        if !field.help_text.is_empty() {
            body = body.push(text(&field.help_text).size(11).color(TEXT_FAINT));
        }
    }

    // 保存 + 回退默认（插件方式下可一键恢复本机 server）
    let mut actions = row![
        button("保存并应用")
            .on_press(Message::ApplySettings)
            .padding([10, 18])
            .style(primary_button),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    if state.adapter == AdapterKind::Plugin {
        actions = actions.push(
            button(
                text("恢复默认（连接本机 server）")
                    .size(12)
                    .color(TEXT_MUTED),
            )
            .on_press(Message::SyncUseServer)
            .padding([10, 12])
            .style(secondary_button),
        );
    }
    body = body.push(actions);
    if !state.plugin_test_status.is_empty() {
        // 测试连接反馈：请求进行中/结果（成功绿字，失败原样带原因）
        body = body.push(text(&state.plugin_test_status).size(12).color(
            if state.plugin_test_status == "连接成功。" {
                GREEN
            } else {
                TEXT_MUTED
            },
        ));
    }
    if !state.settings_status.is_empty() {
        body = body.push(text(&state.settings_status).size(12).color(TEXT_MUTED));
    }
    body.into()
}

/// 插件商店卡片：索引刷新 + clipboard_sync 插件列表 + 一键安装。
fn store_body(state: &UiState) -> Element<'_, Message> {
    let mut body = column![
        row![
            card_title("插件商店"),
            Space::new().width(Length::Fill),
            button(text("刷新索引").size(12))
                .on_press(Message::PluginStoreRefresh)
                .padding([6, 12])
                .style(primary_button),
        ]
        .align_y(Alignment::Center),
        text(if state.store_status.is_empty() {
            "仅展示 clipboard_sync 类型插件。"
        } else {
            &state.store_status
        })
        .size(11)
        .color(TEXT_MUTED),
    ]
    .spacing(8);

    for entry in &state.store_plugins {
        let installed = state.installed_plugins.iter().any(|p| p.id == entry.id);
        let action_btn: Element<'_, Message> = if installed {
            text("已安装").size(12).color(GREEN).into()
        } else {
            button(text("安装").size(12))
                .on_press(Message::PluginStoreInstall(entry.id.clone()))
                .padding([6, 12])
                .style(primary_button)
                .into()
        };
        body = body.push(
            row![
                column![
                    text(format!("{} ({})", entry.name, entry.version))
                        .size(13)
                        .color(TEXT_MAIN),
                    text(&entry.description).size(11).color(TEXT_MUTED),
                ]
                .spacing(2)
                .width(Length::Fill),
                action_btn,
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    if state.store_plugins.is_empty() {
        body = body.push(
            text("暂无条目；点击「刷新索引」从插件中心拉取。")
                .size(12)
                .color(TEXT_FAINT),
        );
    }
    body.into()
}

/// 同步设置卡片：主开关 + 拉取/推送 + 上传白名单 + 大小上限 + 通知。
fn sync_card_body(state: &UiState) -> Element<'_, Message> {
    column![
        card_title("同步"),
        toggle_row(
            "同步总开关",
            state.sync_switch_on,
            Message::SettingsSyncSwitch,
            "关闭时引擎空转，不推不拉。",
        ),
        toggle_row(
            "推送（本地 → 服务器）",
            state.sync_push_on,
            Message::SettingsSyncPush,
            "本地剪贴板变化时上传到服务器。",
        ),
        toggle_row(
            "拉取（服务器 → 本地）",
            state.sync_pull_on,
            Message::SettingsSyncPull,
            "服务器内容变化时下载写回本地。",
        ),
        Space::new().height(4),
        text("上传白名单").size(12).color(TEXT_MUTED),
        toggle_row(
            "文本",
            state.sync_upload_text,
            Message::SettingsSyncUploadText,
            ""
        ),
        toggle_row(
            "图片",
            state.sync_upload_image,
            Message::SettingsSyncUploadImage,
            "框架预留：引擎当前仅同步文本。",
        ),
        toggle_row(
            "单个文件",
            state.sync_upload_single_file,
            Message::SettingsSyncUploadSingleFile,
            "框架预留：引擎当前仅同步文本。",
        ),
        Space::new().height(4),
        field_label("上传大小上限（MB）"),
        TextInput::new("10", &state.sync_max_file_mb)
            .on_input(Message::SettingsSyncMaxFileMb)
            .padding(10)
            .style(input_style),
        Space::new().height(4),
        toggle_row(
            "收到远端内容时通知",
            state.sync_notify,
            Message::SettingsSyncNotify,
            "下载到新内容时提示来源设备。",
        ),
    ]
    .spacing(8)
    .into()
}

/// 开关行：label + toggler + 说明文本。
fn toggle_row<'a>(
    label: &'a str,
    checked: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
    hint: &'a str,
) -> Element<'a, Message> {
    let t = toggler(checked).label(label).on_toggle(on_toggle);
    if hint.is_empty() {
        t.into()
    } else {
        column![t, text(hint).size(11).color(TEXT_FAINT)]
            .spacing(2)
            .into()
    }
}

/// 设置表单字段标签。
fn field_label<'a>(label: &'a str) -> Element<'a, Message> {
    text(label).size(12).color(TEXT_MUTED).into()
}

#[derive(Clone)]
struct RxId(std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<UiMsg>>>);

impl std::hash::Hash for RxId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(&*self.0 as *const _, state);
    }
}

impl PartialEq for RxId {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(&*self.0 as *const _, &*other.0 as *const _)
    }
}

impl Eq for RxId {}

/// 订阅后台事件 channel：轮询接收端，取到事件即产出 Message。
fn backend_events(
    rx: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<UiMsg>>>,
) -> Subscription<Message> {
    use iced::futures::channel::mpsc as fmpsc;
    Subscription::run_with(RxId(rx), |rx| {
        let rx = rx.clone();
        iced::stream::channel(64, move |mut tx: fmpsc::Sender<Message>| async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
            loop {
                interval.tick().await;
                let msg = rx.0.lock().ok().and_then(|g| g.try_recv().ok());
                match msg {
                    Some(msg) => {
                        if tx.try_send(Message::Backend(msg)).is_err() {
                            break;
                        }
                    }
                    None => continue,
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ximed_sync 插件 fixture：与本机 server 关联字段 + 通用字段。
    fn sync_plugin_fixture() -> InstalledPlugin {
        InstalledPlugin {
            id: XIMED_SYNC_PLUGIN_ID.to_string(),
            name: "ximed 剪贴板同步".to_string(),
            version: "0.1.0".to_string(),
            fields: vec![
                PluginFieldSpec {
                    key: "serverUrl".to_string(),
                    label: "服务器地址".to_string(),
                    kind: PluginFieldKind::Text,
                    required: true,
                    placeholder: String::new(),
                    help_text: String::new(),
                },
                PluginFieldSpec {
                    key: "username".to_string(),
                    label: "用户名".to_string(),
                    kind: PluginFieldKind::Text,
                    required: false,
                    placeholder: String::new(),
                    help_text: String::new(),
                },
                PluginFieldSpec {
                    key: "password".to_string(),
                    label: "密码".to_string(),
                    kind: PluginFieldKind::Secret,
                    required: false,
                    placeholder: String::new(),
                    help_text: String::new(),
                },
                PluginFieldSpec {
                    key: "testConnection".to_string(),
                    label: "测试连接".to_string(),
                    kind: PluginFieldKind::Button,
                    required: false,
                    placeholder: String::new(),
                    help_text: String::new(),
                },
            ],
            values: vec![(
                "serverUrl".to_string(),
                "https://saved.example.com".to_string(),
            )],
        }
    }

    fn test_state() -> UiState {
        let (tx, _rx) = ui_channel();
        let (cmd_tx, _cmd_rx) = ui_cmd_channel();
        UiState::new(
            &ClientConfig::default(),
            &crate::settings::AppSettings::default(),
            tx,
            cmd_tx,
            true,
            "/tmp/ximed-test-cfg".to_string(),
        )
    }

    #[test]
    fn ximed_sync_plugin_prefills_empty_fields_from_local_server() {
        let mut state = test_state();
        state.local_server_info = Some(LocalServerInfo {
            url: "http://127.0.0.1:8443".to_string(),
            username: "alice".to_string(),
            password: "local-pw".to_string(),
        });
        state.installed_plugins = vec![sync_plugin_fixture()];

        state.load_plugin_edit_state(XIMED_SYNC_PLUGIN_ID);
        // 已保存值不覆盖；空缺字段填充本地配置；无关字段不动
        let value_of = |key: &str| -> String {
            state
                .plugin_fields
                .iter()
                .position(|f| f.key == key)
                .and_then(|idx| state.plugin_values.get(idx).cloned())
                .unwrap_or_default()
        };
        assert_eq!(value_of("serverUrl"), "https://saved.example.com");
        assert_eq!(value_of("username"), "alice");
        assert_eq!(value_of("password"), "local-pw");
        assert_eq!(value_of("testConnection"), "");
    }

    #[test]
    fn other_plugins_do_not_prefill() {
        let mut state = test_state();
        state.local_server_info = Some(LocalServerInfo {
            url: "http://127.0.0.1:8443".to_string(),
            username: "alice".to_string(),
            password: "local-pw".to_string(),
        });
        let mut plugin = sync_plugin_fixture();
        plugin.id = "com.other.sync".to_string();
        state.installed_plugins = vec![plugin];

        state.load_plugin_edit_state("com.other.sync");
        // 无本地预填：未保存字段保持空（fixture 已保存的 serverUrl 除外）
        assert_eq!(state.plugin_values[0], "https://saved.example.com");
        assert!(state.plugin_values[1..].iter().all(|v| v.is_empty()));
    }

    #[test]
    fn prefill_requires_local_server_info() {
        let mut state = test_state();
        state.installed_plugins = vec![sync_plugin_fixture()];

        state.load_plugin_edit_state(XIMED_SYNC_PLUGIN_ID);
        // 未保存的 username/password 保持空（无本地信息可填充）
        assert!(state.plugin_values[1..].iter().all(|v| v.is_empty()));
    }

    #[test]
    fn format_time_buckets() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_time(now), "刚刚");
        assert_eq!(format_time(now - 30), "刚刚");
        assert_eq!(format_time(now - 120), "2 分钟前");
        assert_eq!(format_time(now - 3600), "1 小时前");
        assert_eq!(format_time(now - 7200), "2 小时前");
        assert_eq!(format_time(now - 86400), "1 天前");
        assert_eq!(format_time(now - 3 * 86400), "3 天前");
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    const WIDTH: f32 = 1024.0;

    #[test]
    fn short_content_not_expandable() {
        assert!(!history_has_more("hello", WIDTH));
        assert!(!history_has_more("short text with a few words", WIDTH));
    }

    #[test]
    fn exactly_three_lines_not_expandable() {
        let text = format!("{}\n{}\n{}", "line one", "line two", "line three");
        assert_eq!(history_estimated_lines(&text, WIDTH), 3);
        assert!(!history_has_more(&text, WIDTH));
    }

    #[test]
    fn more_than_three_lines_expandable() {
        let text = "one\ntwo\nthree\nfour";
        assert_eq!(history_estimated_lines(text, WIDTH), 4);
        assert!(history_has_more(text, WIDTH));
    }

    #[test]
    fn long_wrapped_line_counts_multiple_lines() {
        // 宽 1024 → 内容约 772px → 每行约 110 字符；300 字符应跨 3 行。
        let text = "x".repeat(300);
        let lines = history_estimated_lines(&text, WIDTH);
        assert!(lines >= 3, "expected >=3 lines, got {lines}");
        assert!(history_has_more(&text, WIDTH) || lines <= 3);
    }

    #[test]
    fn empty_text_not_expandable() {
        assert!(!history_has_more("", WIDTH));
        assert!(!history_has_more("\n", WIDTH));
    }
}
