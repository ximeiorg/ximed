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

use std::sync::OnceLock;

use client::config::ClientConfig;
use client::history::HistoryItem;
use iced::font::Weight;
use iced::widget::{
    Space, TextInput, button, column, container, image, row, scrollable, text, text_input, toggler,
};
use iced::{
    Alignment, Background, Border, Color, Element, Font, Length, Subscription, Task, Theme, border,
};

use crate::settings::AppSettings;

/// 面板切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// 概览：同步是否通、谁在同步、总开关。
    Status,
    /// 同步：通道优先级、当前通道配置、同步内容。
    Sync,
    /// 设备：局域网身份、配对、已配对设备。
    Device,
    /// 剪贴板历史。
    History,
    /// 设置：数据目录与通用设置。
    Settings,
    /// 关于：版本、作者、功能、许可证与依赖。
    About,
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
    /// 设置已保存并应用（通道/内容范围已生效）。
    SettingsApplied,
    /// 配对结果（后台配对线程产生）。
    Pairing(client::p2p::PairEvent),
    /// 通道连通性测试结果。
    ConnectionTested { ok: bool, message: String },
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
    /// 打开配置文件所在目录。
    OpenConfigDir,
    /// 应用新设置（通道优先级 / 各通道参数 / 同步内容），持久化并重启 client 引擎。
    ApplySettings(Box<AppSettings>),
}

/// 通道配置字段：一条消息覆盖各通道的所有表单项。
///
/// 不用「每个字段一个 Message」：通道参数会随通道增加而膨胀，逐个加变体既啰嗦
/// 又容易漏改（新增字段要同时改 message / update / view 三处）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelField {
    WebDavUrl,
    WebDavFile,
    WebDavUsername,
    WebDavPassword,
    P2pName,
}

/// Linux 托盘泵间隔：GLib 要驱动，但没必要每帧重建整个 UI。
#[cfg(target_os = "linux")]
const TRAY_PUMP_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// UI 消息（iced Message：channel 事件 + 按钮动作）。
#[derive(Debug, Clone)]
pub enum Message {
    /// 来自后台线程的状态事件。
    Backend(UiMsg),
    /// 窗口已打开（记录 window id，供显示/隐藏控制）。
    WindowOpened(iced::window::Id),
    /// 窗口大小变化（用于历史卡片换行行数估算）。
    WindowResized(iced::Size),
    /// 窗口已关闭（daemon 模式下销毁窗口、进程继续常驻）。
    WindowClosed(iced::window::Id),
    /// 窗口关闭请求（点红叉 → 关闭窗口，托盘里仍可恢复）。
    WindowCloseRequested(iced::window::Id),
    /// 切换到某个面板。
    SelectPanel(Panel),
    /// 展开/收起某条剪贴板历史卡片。
    ToggleHistoryItem(u64),
    /// 打开配置文件所在目录。
    OpenConfigDir,
    /// 退出按钮。
    Quit,
    /// 同步主开关（草稿，点「保存并应用」才生效）。
    SettingsSyncSwitch(bool),
    /// 推送开关（本地 → 通道）。
    SettingsSyncPush(bool),
    /// 拉取开关（通道 → 本地）。
    SettingsSyncPull(bool),
    SettingsSyncUploadText(bool),
    SettingsSyncUploadImage(bool),
    SettingsSyncUploadSingleFile(bool),
    /// 上传大小上限（MB，文本态）。
    SettingsSyncMaxFileMb(String),
    /// 收到远端内容时通知。
    SettingsSyncNotify(bool),
    /// 选中要编辑配置的通道。
    ChannelSelect(String),
    /// 启用 / 停用某个通道（从优先级列表增删）。
    ChannelToggle(String),
    /// 提高通道优先级。
    ChannelMoveUp(String),
    /// 降低通道优先级。
    ChannelMoveDown(String),
    /// 编辑通道参数（一条消息覆盖所有通道表单）。
    ChannelField {
        field: ChannelField,
        value: String,
    },
    /// 测试当前选中通道的连通性。
    TestConnection,
    /// 编辑本机设备名（局域网配对时展示给对方）。
    DeviceName(String),
    /// 生成显示码（本机作为显示端）。
    PairingStartDisplay,
    /// 用对方的码配对（本机作为输入端）。
    PairingStartJoin,
    /// 取消配对 / 重新生成。
    PairingCancel,
    /// 输入对方的配对码。
    PairingInput(String),
    /// 切换配对方向（0 显示 / 1 输入）。
    PairingTab(usize),
    /// 每秒心跳：只用于刷新配对码倒计时。
    Tick,
    /// 驱动 Linux 托盘（GTK/GLib）事件队列；其他平台空转。
    PumpTray,
    /// 移除已配对设备。
    RemovePeer(String),
    /// 保存并应用设置。
    ApplySettings,
}

/// 窗口状态。
#[derive(Debug, Clone)]
pub struct UiState {
    /// 本机来源设备（配置只读）。
    pub source: String,
    /// 内置回退通道依赖的本地服务是否在跑（界面只用于「内置回退」状态）。
    pub server_running: bool,
    /// 引擎状态文本（动态）。
    pub engine_status: String,
    /// 当前面板。
    pub panel: Panel,
    /// 历史列表（动态）。
    pub history: Vec<HistoryItem>,
    /// 展开的历史条目序号（seq；点击卡片展开/收起，只允许一个展开）。
    pub expanded_history: Option<u64>,
    /// 是否显示可写配置的页面（desktop 形态 true，client-only false）。
    pub local_server: bool,
    /// 配置目录路径（数据目录 / 打开文件管理器用）。
    pub config_dir: String,
    /// 设置草稿：所有编辑都写这里，点「保存并应用」才落盘并重建引擎。
    ///
    /// 单一来源替代原先 20 多个平行字段——原来「改了一半的界面」与「已保存的配置」
    /// 混在一起，取消/失败后界面显示的是没生效的值。
    pub draft: AppSettings,
    /// 当前正在编辑配置的通道 id。
    pub selected_channel: String,
    /// 上传大小上限（MB，文本态：输入过程中可能为空）。
    pub max_file_mb: String,
    /// 已配对设备（局域网通道）。
    pub device_peers: Vec<client::p2p::PeerInfo>,
    /// 设备表读取错误（损坏时不静默当作空表）。
    pub device_error: Option<String>,
    /// 显示中的配对码（会话进行中）。
    pub pairing_code: Option<String>,
    /// 显示端会话句柄（取消时用；会话本体在后台线程里阻塞等待）。
    pairing_handle: Option<client::p2p::DisplayPairing>,
    /// 输入中的配对码。
    pub pairing_input: String,
    /// 配对状态文本。
    pub pairing_status: String,
    /// 状态文本是否为失败（决定提示条配色）。
    pub pairing_error: bool,
    /// 配对进行中（禁用按钮，避免重复开端口）。
    pub pairing_busy: bool,
    /// 配对方向：0 = 显示配对码，1 = 输入配对码（对齐 setup 的 tab_bar）。
    pub pairing_tab: usize,
    /// 通道连通性测试结果文本。
    pub channel_test: String,
    /// 测试结果是否成功（着色用）；`None` 表示未测试。
    pub channel_test_ok: Option<bool>,
    /// 设置保存状态提示文本。
    pub settings_status: String,
    /// client 是否已连上通道（引擎上报）。
    pub sync_online: bool,
    /// 后台命令发送端。
    cmd_tx: UiCmdSender,
    /// 后台事件发送端（托盘事件也经此注入）。
    tx: UiSender,
    /// 主窗口 id（首次打开时记录，供显示/隐藏控制）。
    window_id: Option<iced::window::Id>,
    /// 主窗口宽度（逻辑像素），用于估算历史卡片内容是否超过 3 行。
    window_width: f32,
    /// 托盘是否已创建（首个 Backend 消息/窗口打开时创建，主线程）。
    tray_started: bool,
    /// 托盘是否创建成功；失败时关窗必须退出，否则隐藏后无法恢复或退出。
    tray_ok: bool,
    /// 主窗口设置：daemon 模式关窗即销毁，重新显示时按这份设置重建。
    window_settings: iced::window::Settings,
}

impl UiState {
    pub fn new(
        cfg: &ClientConfig,
        app_settings: &AppSettings,
        tx: UiSender,
        cmd_tx: UiCmdSender,
        local_server: bool,
        config_dir: String,
        window_settings: iced::window::Settings,
    ) -> Self {
        let draft = app_settings.clone();
        // 界面只展示局域网 / WebDAV；后台的 HTTP 回退通道不给配置入口
        let selected_channel = draft
            .channels
            .iter()
            .find(|id| id.as_str() != client::channel::CHANNEL_HTTP)
            .cloned()
            .unwrap_or_default();
        let max_file_mb = (draft.sync.max_file_byte / (1024 * 1024)).to_string();
        let mut state = Self {
            source: cfg.source.clone(),
            server_running: false,
            engine_status: "starting…".to_string(),
            panel: Panel::Status,
            history: Vec::new(),
            expanded_history: None,
            local_server,
            config_dir,
            draft,
            selected_channel,
            max_file_mb,
            device_peers: Vec::new(),
            device_error: None,
            pairing_code: None,
            pairing_handle: None,
            pairing_input: String::new(),
            pairing_status: String::new(),
            pairing_error: false,
            pairing_busy: false,
            pairing_tab: 0,
            channel_test: String::new(),
            channel_test_ok: None,
            settings_status: String::new(),
            sync_online: false,
            cmd_tx,
            tx,
            window_id: None,
            window_width: 1024.0,
            tray_started: false,
            tray_ok: false,
            window_settings,
        };
        state.reload_peers();
        state
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

/// 运行桌面 UI：阻塞直到进程退出（关窗只是销毁窗口，daemon 继续常驻托盘）。
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
    // 窗口图标：编译期嵌入 PNG（1024×1024 RGBA），解码失败视为程序 bug。
    let icon = iced::window::icon::from_file_data(include_bytes!("../assets/icon.png"), None)
        .expect("embedded window icon must decode");
    let window_settings = iced::window::Settings {
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
        // 关窗由 update 决定：托盘可用 → close 窗口、daemon 继续常驻；
        // 托盘不可用 → 直接退出，避免没有窗口又无法恢复。
        exit_on_close_request: false,
        ..Default::default()
    };
    let state = UiState::new(
        &cfg,
        &app_settings,
        tx,
        cmd_tx,
        local_server,
        config_dir,
        window_settings.clone(),
    );
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));

    // daemon 模式：默认没有窗口，窗口全部关闭后进程也不退出——这是托盘常驻的基础。
    // 启动时开主窗口；关窗时 Windows/macOS/X11 用 Hidden 保留窗口，
    // Wayland 因 set_visible 是 no-op 而销毁窗口，托盘恢复时按同一份设置重建。
    iced::daemon(
        move || {
            let mut state = state.clone();
            let (id, task) = iced::window::open(window_settings.clone());
            state.window_id = Some(id);
            (state, task.map(Message::WindowOpened))
        },
        update,
        daemon_view,
    )
    .title("ximed")
    .subscription(move |_state| {
        // Linux 分支会 push 托盘泵；其余平台不会，`mut` 在这些平台上多余，
        // 但保留它才能让同一个 vec 跨平台成立（CI 在 Linux 上跑，本地
        // macOS/Windows 跑同一命令时不应该因此报错）。
        #[allow(unused_mut)]
        let mut subscriptions = vec![
            backend_events(rx.clone()),
            window_events(),
            iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::Tick),
        ];
        // Linux 托盘走 GTK/AppIndicator，GLib 主上下文需要被周期性驱动，
        // 否则托盘项不注册、菜单点击没反应（见 tray 模块文档）。
        #[cfg(target_os = "linux")]
        subscriptions.push(iced::time::every(TRAY_PUMP_INTERVAL).map(|_| Message::PumpTray));
        Subscription::batch(subscriptions)
    })
    .theme(move |state: &UiState, _window| Some(theme(state)))
    .run()
}

/// daemon 的 view 适配：窗口 id 不参与视图，界面状态由 `UiState` 统一持有。
fn daemon_view(state: &UiState, _window: iced::window::Id) -> Element<'_, Message> {
    view(state)
}

/// 窗口事件订阅：记录窗口 id、尺寸、关闭请求与销毁事件。
fn window_events() -> Subscription<Message> {
    Subscription::batch([
        iced::window::open_events().map(Message::WindowOpened),
        iced::window::resize_events().map(|(_id, size)| Message::WindowResized(size)),
        iced::window::close_requests().map(Message::WindowCloseRequested),
        iced::window::close_events().map(Message::WindowClosed),
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
    // 首个后台事件 / 窗口打开 / Linux 托盘泵 时在主线程创建托盘：
    // 此时事件循环已运行，GTK/NSStatusItem 才能挂上去。
    if matches!(
        &message,
        Message::Backend(_) | Message::WindowOpened(_) | Message::PumpTray
    ) {
        state.ensure_tray();
    }
    match message {
        Message::Backend(UiMsg::EngineStarted { source }) => {
            state.engine_status = format!("engine running (device {source})");
        }
        Message::Backend(UiMsg::EngineStopped { source }) => {
            state.engine_status = format!("engine stopped (device {source})");
        }
        Message::Backend(UiMsg::ServerListening { .. }) => {
            state.server_running = true;
        }
        Message::Backend(UiMsg::ServerStopped { .. }) => {
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
        Message::Backend(UiMsg::Pairing(event)) => match event {
            client::p2p::PairEvent::Paired { name, .. } => {
                state.pairing_error = false;
                state.pairing_status = format!("已与「{name}」配对，可以开始同步了");
                state.pairing_code = None;
                state.pairing_handle = None;
                state.pairing_busy = false;
                state.pairing_input.clear();
                state.reload_peers();
            }
            client::p2p::PairEvent::Failed(error) => {
                state.pairing_error = true;
                state.pairing_status = error;
                state.pairing_code = None;
                state.pairing_handle = None;
                state.pairing_busy = false;
            }
        },
        Message::Backend(UiMsg::ConnectionTested { ok, message }) => {
            state.channel_test = message;
            state.channel_test_ok = Some(ok);
        }
        Message::Backend(UiMsg::ShowWindow) => {
            return show_window(state);
        }
        Message::Backend(UiMsg::HideWindow) => {
            return hide_window(state);
        }
        Message::Backend(UiMsg::Quit) => {
            crate::tray::shutdown();
            return iced::exit();
        }
        Message::WindowOpened(id) => {
            state.window_id = Some(id);
            // 托盘恢复时新窗口可能落在其他窗口后面；打开即聚焦。
            return iced::window::gain_focus(id);
        }
        Message::WindowClosed(id) => {
            if state.window_id == Some(id) {
                state.window_id = None;
            }
        }
        Message::WindowResized(size) => {
            state.window_width = size.width;
            // Wayland 关窗会销毁窗口，记住最后一次尺寸，托盘恢复时按它重建。
            state.window_settings.size = size;
        }
        Message::WindowCloseRequested(id) => {
            state.window_id = Some(id);
            // 关闭按钮：hide_window 按平台隐藏/销毁窗口，daemon 下进程继续常驻。
            // 托盘不可用时必须退出，否则窗口消失后用户既看不到界面也无法退出。
            state.ensure_tray();
            if state.tray_ok {
                return hide_window(state);
            }
            crate::tray::shutdown();
            return iced::exit();
        }
        Message::SelectPanel(panel) => {
            state.panel = panel;
            if panel == Panel::Device {
                state.reload_peers();
            }
        }
        Message::ToggleHistoryItem(seq) => {
            state.expanded_history = if state.expanded_history == Some(seq) {
                None
            } else {
                Some(seq)
            };
        }
        Message::OpenConfigDir => {
            let _ = state.cmd_tx.send(UiCmd::OpenConfigDir);
        }
        Message::SettingsSyncSwitch(on) => state.edit_sync(|s| s.switch_on = on),
        Message::SettingsSyncPush(on) => state.edit_sync(|s| s.push_switch_on = on),
        Message::SettingsSyncPull(on) => state.edit_sync(|s| s.pull_switch_on = on),
        Message::SettingsSyncUploadText(on) => state.edit_sync(|s| s.upload_text = on),
        Message::SettingsSyncUploadImage(on) => state.edit_sync(|s| s.upload_image = on),
        Message::SettingsSyncUploadSingleFile(on) => state.edit_sync(|s| s.upload_single_file = on),
        Message::SettingsSyncMaxFileMb(v) => {
            // 中间态允许为空（用户清空重输），但只接受数字
            if v.is_empty() || v.parse::<u64>().is_ok() {
                state.max_file_mb = v;
                state.settings_status.clear();
            }
        }
        Message::SettingsSyncNotify(on) => state.edit_sync(|s| s.notify_on_downloaded = on),
        Message::ChannelSelect(id) => {
            state.selected_channel = id;
            state.channel_test.clear();
            state.channel_test_ok = None;
        }
        Message::ChannelToggle(id) => state.toggle_channel(&id),
        Message::ChannelMoveUp(id) => state.move_channel(&id, true),
        Message::ChannelMoveDown(id) => state.move_channel(&id, false),
        Message::ChannelField { field, value } => state.edit_channel_field(field, value),
        Message::TestConnection => state.test_connection(),
        Message::DeviceName(name) => {
            state.draft.p2p.device_name = name;
            state.settings_status.clear();
        }
        Message::PairingStartDisplay => state.start_display_pairing(),
        Message::PairingStartJoin => state.start_join_pairing(),
        Message::PairingCancel => state.cancel_pairing(),
        Message::PairingInput(code) => state.pairing_input = code,
        Message::PairingTab(tab) => {
            state.pairing_tab = tab.min(1);
            state.pairing_status.clear();
        }
        Message::Tick => {
            // 码 60 秒过期：到点自动停会话，避免界面显示一个已经作废的码
            let expired = state
                .pairing_handle
                .as_ref()
                .is_some_and(|handle| handle.remaining_secs() == 0);
            if expired {
                state.pairing_handle = None;
                state.pairing_code = None;
                state.pairing_busy = false;
                state.pairing_status = "配对码已过期，请重新生成".to_string();
                state.pairing_error = true;
            }
        }
        Message::PumpTray => crate::tray::pump(),
        Message::RemovePeer(did) => state.remove_peer(&did),
        Message::ApplySettings => {
            let cmd = UiCmd::ApplySettings(Box::new(state.saved_draft()));
            if state.cmd_tx.send(cmd).is_ok() {
                state.settings_status = "正在保存并应用…".to_string();
            } else {
                state.settings_status = "保存失败：后台通道不可用。".to_string();
            }
        }
        Message::Quit => {
            crate::tray::shutdown();
            return iced::exit();
        }
    }
    Task::none()
}

impl UiState {
    /// 修改同步内容草稿（任何改动都清掉「已保存」提示）。
    fn edit_sync(&mut self, f: impl FnOnce(&mut client::settings::SyncSettings)) {
        f(&mut self.draft.sync);
        self.settings_status.clear();
    }

    /// 待保存的设置：把界面上的文本态（大小上限）折回字节，并规范化通道列表。
    fn saved_draft(&self) -> AppSettings {
        let mut draft = self.draft.clone();
        draft.channels = draft.normalized_channels();
        draft.sync.max_file_byte = self
            .max_file_mb
            .parse::<u64>()
            .ok()
            .map(|mb| mb.saturating_mul(1024 * 1024))
            .filter(|bytes| *bytes > 0)
            .unwrap_or(10 * 1024 * 1024);
        draft
    }

    /// 界面上可见的通道：HTTP 是后台回退通道，不提供配置入口。
    fn visible_channels(&self) -> Vec<String> {
        self.draft
            .channels
            .iter()
            .filter(|id| id.as_str() != client::channel::CHANNEL_HTTP)
            .cloned()
            .collect()
    }

    /// 重新读取已配对设备表。
    ///
    /// 读失败时保留错误而不是展示空表：表里是用户建立的信任关系，显示「暂无设备」
    /// 会让人误以为配对丢了。
    fn reload_peers(&mut self) {
        match client::p2p::peers(&self.config_dir) {
            Ok(list) => {
                self.device_peers = list;
                self.device_error = None;
            }
            Err(e) => {
                self.device_peers.clear();
                self.device_error = Some(e);
            }
        }
    }

    /// 启用 / 停用通道（从优先级列表增删）。
    fn toggle_channel(&mut self, id: &str) {
        // HTTP 回退通道由后台维护，界面既不能加也不能删
        if id == client::channel::CHANNEL_HTTP {
            return;
        }
        if let Some(pos) = self.draft.channels.iter().position(|c| c == id) {
            if self.draft.channels.len() == 1 {
                self.settings_status = "至少保留一条通道，否则同步无法启动。".to_string();
                return;
            }
            self.draft.channels.remove(pos);
            if self.selected_channel == id {
                self.selected_channel = self.draft.channels.first().cloned().unwrap_or_default();
            }
        } else {
            // 新加的通道放到最前：用户主动加一条通道，意图通常是「改用它」，
            // 排到末尾会变成「配好了却没生效」——看起来像 bug。
            self.draft.channels.insert(0, id.to_string());
            self.selected_channel = id.to_string();
        }
        self.channel_test.clear();
        self.channel_test_ok = None;
        self.settings_status.clear();
    }

    /// 调整优先级（`up = true` 上移一位）。
    fn move_channel(&mut self, id: &str, up: bool) {
        let Some(pos) = self.draft.channels.iter().position(|c| c == id) else {
            return;
        };
        let target = if up {
            pos.checked_sub(1)
        } else {
            Some(pos + 1).filter(|next| *next < self.draft.channels.len())
        };
        let Some(target) = target else {
            return;
        };
        self.draft.channels.swap(pos, target);
        self.settings_status.clear();
    }

    /// 编辑通道参数。
    fn edit_channel_field(&mut self, field: ChannelField, value: String) {
        match field {
            ChannelField::WebDavUrl => self.draft.webdav.url = value,
            ChannelField::WebDavFile => self.draft.webdav.file = value,
            ChannelField::WebDavUsername => self.draft.webdav.username = value,
            ChannelField::WebDavPassword => {
                self.draft.webdav.password = Some(value).filter(|p| !p.is_empty())
            }
            ChannelField::P2pName => self.draft.p2p.device_name = value,
        }
        self.channel_test.clear();
        self.channel_test_ok = None;
        self.settings_status.clear();
    }

    /// 测试当前选中通道。
    ///
    /// WebDAV 要发真实请求，放后台线程；局域网只查设备表（连接态由引擎维护）。
    fn test_connection(&mut self) {
        let id = self.selected_channel.clone();
        if id == client::channel::CHANNEL_P2P {
            match client::p2p::peers(&self.config_dir) {
                Ok(list) if list.is_empty() => {
                    self.channel_test = "还没有配对设备，请先完成配对".to_string();
                    self.channel_test_ok = Some(false);
                }
                Ok(list) => {
                    self.channel_test = format!("已配对 {} 台设备", list.len());
                    self.channel_test_ok = Some(true);
                }
                Err(e) => {
                    self.channel_test = format!("设备表读取失败：{e}");
                    self.channel_test_ok = Some(false);
                }
            }
            return;
        }
        self.channel_test = "正在测试…".to_string();
        self.channel_test_ok = None;
        let webdav = self.draft.webdav.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let transport = client::webdav::WebDavTransport::new(&webdav);
            let outcome = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(transport.test_connection()),
                Err(e) => Err(client::transport::TransportError::Connect(e.to_string())),
            };
            let (ok, message) = match outcome {
                Ok(()) => (true, "连接成功".to_string()),
                Err(e) => (false, format!("连接失败：{e}")),
            };
            let _ = tx.send(UiMsg::ConnectionTested { ok, message });
        });
    }

    /// 生成显示码（本机作为显示端）。
    fn start_display_pairing(&mut self) {
        if self.pairing_busy {
            return;
        }
        let base = self.config_dir.clone();
        let name = self.device_name();
        let tx = self.tx.clone();
        self.pairing_busy = true;
        self.pairing_tab = 0;
        self.pairing_error = false;
        self.pairing_status = "已生成配对码，请在对方设备上输入".to_string();
        match client::p2p::start_display_pairing(&base, name, move |event| {
            let _ = tx.send(UiMsg::Pairing(event));
        }) {
            Ok(handle) => {
                self.pairing_code = Some(handle.code().to_string());
                self.pairing_handle = Some(handle);
            }
            Err(e) => {
                self.pairing_status = e;
                self.pairing_error = true;
                self.pairing_code = None;
                self.pairing_busy = false;
            }
        }
    }

    /// 用对方的码配对（本机作为输入端）。
    fn start_join_pairing(&mut self) {
        if self.pairing_busy {
            return;
        }
        let code = self.pairing_input.trim().to_string();
        if code.is_empty() {
            self.pairing_status = "请先输入对方屏幕上的配对码".to_string();
            return;
        }
        let base = self.config_dir.clone();
        let name = self.device_name();
        let tx = self.tx.clone();
        self.pairing_busy = true;
        self.pairing_tab = 1;
        self.pairing_error = false;
        self.pairing_status = "正在寻找对方设备…".to_string();
        if let Err(e) = client::p2p::start_join_pairing(&base, name, code, move |event| {
            let _ = tx.send(UiMsg::Pairing(event));
        }) {
            self.pairing_status = e;
            self.pairing_error = true;
            self.pairing_busy = false;
        }
    }

    /// 取消配对（同时释放配对端口）。
    fn cancel_pairing(&mut self) {
        if let Some(handle) = self.pairing_handle.take() {
            handle.cancel();
        }
        self.pairing_code = None;
        self.pairing_busy = false;
        self.pairing_status = "已取消配对".to_string();
    }

    /// 移除已配对设备。
    fn remove_peer(&mut self, did: &str) {
        match client::p2p::remove_peer(&self.config_dir, did) {
            Ok(_) => {
                self.pairing_status = "已移除该设备，对方需要重新配对".to_string();
                self.reload_peers();
            }
            Err(e) => self.pairing_status = format!("移除失败：{e}"),
        }
    }

    /// 本机设备名（空则用主机名）。
    fn device_name(&self) -> String {
        let name = self.draft.p2p.device_name.trim();
        if name.is_empty() {
            client::config::hostname().unwrap_or_else(|| "ximed".to_string())
        } else {
            name.to_string()
        }
    }

    /// 创建托盘（仅一次）。必须在主线程、事件循环已运行时调用。
    fn ensure_tray(&mut self) {
        if self.tray_started {
            return;
        }
        self.tray_started = true;
        #[cfg(target_os = "macos")]
        crate::macos::set_dock_icon();
        match crate::tray::setup(self.tx.clone()) {
            Ok(()) => self.tray_ok = true,
            Err(e) => {
                self.tray_ok = false;
                eprintln!("[ximed] tray setup failed: {e}");
                tracing::warn!("tray setup failed: {e}");
            }
        }
    }
}

/// 显示主窗口（从托盘恢复）：已有窗口就恢复并聚焦，没有就按保存的设置重建。
fn show_window(state: &mut UiState) -> Task<Message> {
    if let Some(id) = state.window_id {
        return Task::batch([
            iced::window::set_mode(id, iced::window::Mode::Windowed),
            iced::window::gain_focus(id),
        ]);
    }
    let (id, task) = iced::window::open(state.window_settings.clone());
    state.window_id = Some(id);
    task.map(move |_| Message::WindowOpened(id))
}

/// 关闭主窗口到托盘（daemon 下进程继续常驻）。
fn hide_window(state: &mut UiState) -> Task<Message> {
    let Some(id) = state.window_id else {
        return Task::none();
    };
    if should_recreate_window() {
        // Wayland 没有隐藏窗口的协议（winit 的 set_visible 是 no-op），
        // 只能销毁窗口，托盘恢复时再重建。
        state.window_id = None;
        return iced::window::close(id);
    }
    iced::window::set_mode(id, iced::window::Mode::Hidden)
}

/// 关窗时是「隐藏」还是「销毁重建」。
///
/// Windows / macOS / X11 的 Hidden 模式可用，保留窗口能保住尺寸与界面状态；
/// Wayland 上 `Window::set_visible` 是空实现，窗口藏不掉，只能close。
fn should_recreate_window() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("WAYLAND_SOCKET").is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

// ---------------------------------------------------------------------------
// 现代化样式：Tailwind 风格（zinc 深色 + indigo 主色，圆角卡片、胶囊、柔和分层）
// ---------------------------------------------------------------------------

/// 主题颜色常量（zinc 色阶 + indigo 强调色，贴近 Tailwind dark UI）。
const BG: Color = Color::from_rgb8(0x09, 0x09, 0x0b); // 窗口底色 zinc-950
const SURFACE: Color = Color::from_rgb8(0x14, 0x14, 0x17); // 卡片/侧栏 zinc-900
const SURFACE_HOVER: Color = Color::from_rgb8(0x23, 0x23, 0x27); // 悬停 zinc-800
const SURFACE_VARIANT: Color = Color::from_rgb8(0x23, 0x23, 0x27); // 输入框填充
/// 分段控件轨道：比窗口底色再深一档，否则整条轨道和页面糊在一起。
const TRACK: Color = Color::from_rgb8(0x26, 0x26, 0x2b);
/// 分段控件选中项（药丸）底色。
const TAB_ACTIVE: Color = Color::from_rgb8(0x3a, 0x3a, 0x41);
/// 次要按钮底色：浮在页面和卡片上都要看得见，所以不用 surface。
const CONTROL_BG: Color = Color::from_rgb8(0x21, 0x21, 0x26);
const CONTROL_BORDER: Color = Color::from_rgb8(0x35, 0x35, 0x3c);
const BORDER: Color = Color::from_rgb8(0x29, 0x29, 0x30); // 边框 zinc-800
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
const RADIUS_LG: f32 = 14.0;
const RADIUS_MD: f32 = 12.0;
const RADIUS_SM: f32 = 8.0;
/// 控件统一高度：输入框、按钮、分段控件都按这一档排版，同一行不会高低错开。
const CONTROL_HEIGHT: f32 = 36.0;

/// 标题字体（semibold）。
const SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..Font::DEFAULT
};

/// 卡片容器样式：圆角 + 边框 + 柔和底色 + 极浅投影（深色底上靠不透明度代替）。
fn card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT_MAIN),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::radius(RADIUS_LG),
        },
        shadow: iced::Shadow {
            color: Color::from_rgba8(0x00, 0x00, 0x00, 0.45),
            offset: iced::Vector::new(0.0, 1.0),
            blur_radius: 3.0,
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

/// 次要操作按钮：透明底 + 边框，悬停浅色底。
fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hovered {
            SURFACE_HOVER
        } else {
            CONTROL_BG
        })),
        text_color: TEXT_MAIN,
        border: Border {
            color: CONTROL_BORDER,
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
fn input_style(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let (border_color, border_width) = match status {
        text_input::Status::Focused { .. } => (ACCENT, 1.5),
        text_input::Status::Hovered => (BORDER_STRONG, 1.0),
        _ => (Color::TRANSPARENT, 1.0),
    };
    text_input::Style {
        background: Background::Color(SURFACE_VARIANT),
        border: Border {
            color: border_color,
            width: border_width,
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
    // 侧栏：品牌 + 平铺导航。条目不多，分组标题只会制造噪声
    let mut nav = column![brand(), Space::new().height(20)]
        .spacing(2)
        .padding([20, 14]);
    nav = nav.push(nav_button("概览", Panel::Status, state.panel));
    // 通道/设备/设置页需要写配置，仅 desktop 形态显示（client-only 无后台写入口）
    if state.local_server {
        nav = nav.push(nav_button("同步通道", Panel::Sync, state.panel));
        nav = nav.push(nav_button("设备", Panel::Device, state.panel));
    }
    nav = nav.push(nav_button("剪贴板历史", Panel::History, state.panel));
    if state.local_server {
        nav = nav.push(nav_button("设置", Panel::Settings, state.panel));
    }
    // 普通导航项；desktop / client-only 都可访问
    nav = nav.push(nav_button("关于", Panel::About, state.panel));

    let sidebar = container(nav)
        .width(200)
        .height(Length::Fill)
        .style(sidebar_style);

    // 右分栏：按面板渲染内容
    let content = match state.panel {
        Panel::Status => overview_view(state),
        Panel::Sync => sync_view(state),
        Panel::Device => device_view(state),
        Panel::History => history_view(state),
        Panel::Settings => settings_view(state),
        Panel::About => about_view(state),
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

/// 侧栏 logo 的图片句柄：整个进程只创建一次。
///
/// `image::Handle::from_bytes` 每次都会生成新的 `Id`，渲染器会据此重新解码/上传
/// 纹理；如果在每个 `view()` 里新建，任何重绘都会让 logo 闪一下。
fn sidebar_logo() -> image::Handle {
    static LOGO: OnceLock<image::Handle> = OnceLock::new();
    LOGO.get_or_init(|| image::Handle::from_bytes(include_bytes!("../assets/icon.png").as_slice()))
        .clone()
}

/// 品牌区：应用图标 + 名称 + 副标题。
fn brand() -> Element<'static, Message> {
    // 编译期嵌入应用图标（1024×1024 RGBA），与窗口图标同源。
    let logo = image(sidebar_logo()).width(28).height(28);
    row![
        logo,
        column![
            text("ximed").size(15).font(SEMIBOLD).color(TEXT_MAIN),
            text("剪贴板同步").size(12).color(TEXT_MUTED),
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
            // 选中态不用改字重：Linux 的 CJK sans 往往没有 Semibold 面，
            // fontconfig 会替换成 Bold（甚至别的族），看起来像换了字体。
            // 主色 + 实心圆点 + 浅底已经足够表达选中。
            text(label)
                .size(14)
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

fn overview_view(state: &UiState) -> Element<'_, Message> {
    let visible = state.visible_channels();
    let primary = visible.first().map(|id| channel_label(id));
    let online = state.sync_online;
    let connection_badge = if online {
        badge("已连接", GREEN, GREEN_DIM)
    } else {
        badge("未连接", RED, RED_DIM)
    };
    let connection_desc = if online {
        "引擎当前可用"
    } else if primary.is_some() {
        "检查通道参数，或换用其它通道"
    } else {
        "先在「同步通道」里选一种方式"
    };

    let (mode_badge, mode_desc) = match &primary {
        Some(name) => (
            badge(name.clone(), ACCENT, ACCENT_DIM),
            "按「同步通道」页的顺序，首个可用通道".to_string(),
        ),
        None => (
            badge("未选择", TEXT_MUTED, SURFACE_VARIANT),
            "选择局域网直连或 WebDAV 后保存即可开始同步".to_string(),
        ),
    };

    let mut items: Vec<Element<'_, Message>> = vec![
        settings_item("同步方式", Some(mode_desc), mode_badge),
        settings_item("连接状态", Some(connection_desc), connection_badge),
        settings_item(
            "同步总开关",
            Some("关闭时引擎空转，不推不拉"),
            switch_control(state.draft.sync.switch_on, Message::SettingsSyncSwitch),
        ),
        settings_item(
            "已配对设备",
            Some("局域网直连的对端数量"),
            text(format!("{} 台", state.device_peers.len()))
                .size(13)
                .color(TEXT_MUTED)
                .into(),
        ),
        settings_item(
            "本机设备",
            Some("写入同步内容的来源标识"),
            text(state.source.clone()).size(13).color(TEXT_MUTED).into(),
        ),
    ];

    // 还没选同步方式时如实说明：后台有一条内置通道在工作，但它不提供配置
    if primary.is_none() {
        items.push(settings_item(
            "内置回退",
            Some("无需配置；选中同步方式并保存后自动停用"),
            text(if state.server_running {
                "运行中"
            } else {
                "未就绪"
            })
            .size(13)
            .color(TEXT_MUTED)
            .into(),
        ));
    }

    let mut content = column![
        panel_header("概览", "同步是否通、谁在同步"),
        settings_group("同步状态", None::<&str>, items),
    ]
    .spacing(16);

    if !state.settings_status.is_empty() {
        content = content.push(banner(&state.settings_status, Tone::Info));
    }

    container(scrollable(content).height(Length::Fill))
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

// ---------------------------------------------------------------------------
// 同步页：通道优先级 / 通道配置 / 同步内容
// ---------------------------------------------------------------------------

fn sync_view(state: &UiState) -> Element<'_, Message> {
    let mut list = column![].spacing(8);
    let mut visible_count = 0usize;
    for (index, id) in state
        .draft
        .channels
        .iter()
        .filter(|id| id.as_str() != client::channel::CHANNEL_HTTP)
        .enumerate()
    {
        list = list.push(channel_row(state, id.as_str(), index));
        visible_count += 1;
    }
    if visible_count == 0 {
        list = list.push(
            container(
                text("还没有选择同步方式。选一种并保存后，剪贴板就开始同步。")
                    .size(13)
                    .color(TEXT_MUTED),
            )
            .padding([8, 4]),
        );
    }

    let mut add_row = row![].spacing(8).align_y(Alignment::Center);
    for id in [
        client::channel::CHANNEL_P2P,
        client::channel::CHANNEL_WEBDAV,
    ] {
        if !state.draft.channels.iter().any(|enabled| enabled == id) {
            add_row = add_row.push(
                button(text(format!("＋ {}", channel_label(id))))
                    .on_press(Message::ChannelToggle(id.to_string()))
                    .padding([6, 12])
                    .style(secondary_button),
            );
        }
    }

    let channels_card = container(
        column![
            card_title("同步方式"),
            text("从上到下依次尝试：局域网直连掉线超过 10 秒会自动切到备用通道，恢复后自动切回。")
                .size(11)
                .color(TEXT_FAINT),
            list,
            add_row,
        ]
        .spacing(10)
        .padding(18),
    )
    .width(Length::Fill)
    .style(card);

    let mut body = column![
        panel_header("同步通道", "选方式、配参数、定范围"),
        Space::new().height(8),
        channels_card,
    ]
    .spacing(16);

    // 只有选中了可见通道时才显示其配置
    if !state.selected_channel.is_empty() {
        body = body.push(channel_config_card(state));
    }
    body = body.push(sync_content_card(state));
    body = body.push(
        row![
            button("保存并应用")
                .on_press(Message::ApplySettings)
                .padding([9, 16])
                .style(primary_button),
        ]
        .align_y(Alignment::Center),
    );
    if !state.settings_status.is_empty() {
        body = body.push(text(&state.settings_status).size(12).color(TEXT_MUTED));
    }

    container(scrollable(body).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 通道行：名称 + 状态摘要 + 优先级标记 + 上移/下移/配置/移除。
fn channel_row<'a>(state: &'a UiState, id: &'a str, index: usize) -> Element<'a, Message> {
    let selected = state.selected_channel == id;
    let priority = if index == 0 {
        "首选".to_string()
    } else {
        format!("备用 {index}")
    };
    let visible_len = state.visible_channels().len();

    let mut actions = row![].spacing(6).align_y(Alignment::Center);
    if index > 0 {
        actions = actions.push(
            button("↑")
                .on_press(Message::ChannelMoveUp(id.to_string()))
                .padding([4, 9])
                .style(secondary_button),
        );
    }
    if index + 1 < visible_len {
        actions = actions.push(
            button("↓")
                .on_press(Message::ChannelMoveDown(id.to_string()))
                .padding([4, 9])
                .style(secondary_button),
        );
    }
    actions = actions.push(
        button(if selected { "配置中" } else { "配置" })
            .on_press(Message::ChannelSelect(id.to_string()))
            .padding([4, 10])
            .style(if selected {
                primary_button
            } else {
                secondary_button
            }),
    );
    // 允许清空到「未选择」，但至少不能把唯一一条删成空列表之外的状态
    if visible_len >= 1 {
        actions = actions.push(
            button("移除")
                .on_press(Message::ChannelToggle(id.to_string()))
                .padding([4, 10])
                .style(danger_button),
        );
    }

    container(
        row![
            column![
                text(channel_label(id))
                    .size(13)
                    .font(SEMIBOLD)
                    .color(TEXT_MAIN),
                text(channel_state_hint(state, id))
                    .size(11)
                    .color(TEXT_FAINT),
            ]
            .spacing(2)
            .width(Length::Fill),
            text(priority)
                .size(11)
                .color(if index == 0 { ACCENT } else { TEXT_MUTED }),
            actions,
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([8, 10])
    .style(card)
    .into()
}

/// 通道行的一行状态摘要（不查网络，只反映配置是否齐全）。
fn channel_state_hint(state: &UiState, id: &str) -> String {
    match client::channel::Channel::from_id(id) {
        Some(client::channel::Channel::WebDav) => {
            if state.draft.webdav.is_configured() {
                format!("保存到 {}", state.draft.webdav.endpoint())
            } else {
                "尚未填写地址".to_string()
            }
        }
        Some(client::channel::Channel::P2p) => {
            if state.device_peers.is_empty() {
                "还没有配对设备".to_string()
            } else {
                format!("已配对 {} 台设备", state.device_peers.len())
            }
        }
        // 其余通道不在界面上出现
        _ => String::new(),
    }
}

/// 选中通道的配置表单（对齐 setup 的 settings_group / settings_item 布局）。
fn channel_config_card(state: &UiState) -> Element<'_, Message> {
    let channel = client::channel::Channel::from_id(&state.selected_channel);

    let (title, description, items): (&str, &str, Vec<Element<'_, Message>>) = match channel {
        Some(client::channel::Channel::WebDav) => (
            "WebDAV",
            "把剪贴板当作网盘上的一个 JSON 文件，最后写入者生效",
            vec![
                settings_item(
                    "服务器地址",
                    Some("WebDAV 目录，例如 https://dav.jianguoyun.com/dav/ximed"),
                    text_control(
                        &state.draft.webdav.url,
                        "https://dav.jianguoyun.com/dav/ximed",
                        move |value| Message::ChannelField {
                            field: ChannelField::WebDavUrl,
                            value,
                        },
                    ),
                ),
                settings_item(
                    "文件名",
                    Some("目录下的文件名，两台设备必须一致"),
                    text_control(&state.draft.webdav.file, "clipboard.json", move |value| {
                        Message::ChannelField {
                            field: ChannelField::WebDavFile,
                            value,
                        }
                    }),
                ),
                settings_item(
                    "用户名",
                    Some("留空表示匿名访问"),
                    text_control(&state.draft.webdav.username, "", move |value| {
                        Message::ChannelField {
                            field: ChannelField::WebDavUsername,
                            value,
                        }
                    }),
                ),
                settings_item(
                    "密码",
                    Some("也可以设置环境变量 XIMED_WEBDAV_PASSWORD"),
                    password_control(
                        state.draft.webdav.password.as_deref().unwrap_or_default(),
                        "",
                        move |value| Message::ChannelField {
                            field: ChannelField::WebDavPassword,
                            value,
                        },
                    ),
                ),
                test_item(state),
            ],
        ),
        Some(client::channel::Channel::P2p) => {
            let peers = if state.device_peers.is_empty() {
                "尚未配对任何设备".to_string()
            } else {
                format!("已配对 {} 台设备", state.device_peers.len())
            };
            (
                "局域网直连",
                "两台设备在同一网络时直接传输，不经过任何服务器",
                vec![
                    settings_item(
                        "设备名",
                        Some("对方在配对时看到的名字，例如「办公室 Mac」"),
                        text_control(
                            &state.draft.p2p.device_name,
                            &state.source,
                            Message::DeviceName,
                        ),
                    ),
                    settings_item(
                        "已配对设备",
                        Some(peers),
                        button("去配对…")
                            .on_press(Message::SelectPanel(Panel::Device))
                            .padding([8, 14])
                            .style(secondary_button)
                            .into(),
                    ),
                    test_item(state),
                ],
            )
        }
        // 其它通道不在界面上提供；选中为空时给中性提示
        _ => (
            "同步方式",
            "先在上面的列表里选择一种同步方式",
            vec![text("").into()],
        ),
    };

    settings_group(title, Some(description), items)
}

/// 「测试连接」条目：WebDAV 发请求、局域网查设备表。
fn test_item(state: &UiState) -> Element<'_, Message> {
    let mut control = row![].spacing(10).align_y(Alignment::Center);
    let needs_config = state.selected_channel == client::channel::CHANNEL_WEBDAV
        && !state.draft.webdav.is_configured();
    if needs_config {
        control = control.push(text("先填写服务器地址").size(12).color(TEXT_FAINT));
    } else {
        control = control.push(
            button("测试连接")
                .on_press(Message::TestConnection)
                .padding([8, 14])
                .style(secondary_button),
        );
    }
    if !state.channel_test.is_empty() {
        control = control.push(text(&state.channel_test).size(12).color(
            match state.channel_test_ok {
                Some(true) => GREEN,
                Some(false) => RED,
                None => TEXT_MUTED,
            },
        ));
    }
    settings_item(
        "连通性",
        Some("只验证配置能否连上，不会立即同步"),
        control.into(),
    )
}

/// 同步内容：开关 + 白名单 + 大小上限 + 通知。
fn sync_content_card(state: &UiState) -> Element<'_, Message> {
    let items: Vec<Element<'_, Message>> = vec![
        toggle_item(
            "同步总开关",
            Some("关闭时引擎空转，不推不拉"),
            state.draft.sync.switch_on,
            Message::SettingsSyncSwitch,
        ),
        toggle_item(
            "推送",
            Some("本机剪贴板变化时上传"),
            state.draft.sync.push_switch_on,
            Message::SettingsSyncPush,
        ),
        toggle_item(
            "拉取",
            Some("远端内容变化时下载并写回本机"),
            state.draft.sync.pull_switch_on,
            Message::SettingsSyncPull,
        ),
        settings_item(
            "文本",
            Some("纯文本剪贴板"),
            switch_control(
                state.draft.sync.upload_text,
                Message::SettingsSyncUploadText,
            ),
        ),
        settings_item(
            "图片",
            Some("局域网通道单帧上限 1 MiB，大图可能发不出去"),
            switch_control(
                state.draft.sync.upload_image,
                Message::SettingsSyncUploadImage,
            ),
        ),
        settings_item(
            "单个文件",
            Some("引擎当前只同步文本与图片"),
            switch_control(
                state.draft.sync.upload_single_file,
                Message::SettingsSyncUploadSingleFile,
            ),
        ),
        settings_item(
            "大小上限（MB）",
            Some("超过此大小的内容不上传"),
            TextInput::new("10", &state.max_file_mb)
                .on_input(Message::SettingsSyncMaxFileMb)
                .padding([8, 10])
                .style(input_style)
                .width(Length::Fixed(120.0))
                .into(),
        ),
        toggle_item(
            "收到内容时通知",
            Some("下载到新内容时提示来源设备"),
            state.draft.sync.notify_on_downloaded,
            Message::SettingsSyncNotify,
        ),
    ];
    settings_group("同步内容", Some("推送 / 拉取方向与白名单"), items)
}

// ---------------------------------------------------------------------------
// 设备页：本机身份 / 配对 / 已配对设备
// ---------------------------------------------------------------------------

fn device_view(state: &UiState) -> Element<'_, Message> {
    let did = client::p2p::local_did()
        .map(|d| short_did(&d))
        .unwrap_or_else(|| "节点未启动".to_string());
    let port = client::p2p::local_port()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "—".to_string());

    let identity = settings_group(
        "本机",
        Some("这一页只显示本机身份与配对状态，配对操作立即生效，不需要保存"),
        vec![
            settings_item(
                "设备名",
                Some("对方在配对时看到的名字；在「同步通道 → 局域网直连」里修改"),
                text(state.device_name()).size(13).color(TEXT_MUTED).into(),
            ),
            settings_item(
                "设备标识",
                Some("由本机密钥派生，两台设备可对照确认是同一台机器"),
                text(did).size(13).color(TEXT_MUTED).into(),
            ),
            settings_item(
                "监听端口",
                Some("配对期间会暂停节点（与同步抢同一组端口），结束后自动恢复"),
                text(port).size(13).color(TEXT_MUTED).into(),
            ),
        ],
    );

    let mut pair_items: Vec<Element<'_, Message>> = vec![tab_bar(
        &["显示配对码", "输入配对码"],
        state.pairing_tab,
        Message::PairingTab,
    )];
    // 另一个方向还在跑时提示：否则用户会以为点了没生效，反复点，既占端口又更慌
    if state.pairing_tab == 1 && state.pairing_code.is_some() {
        pair_items.push(banner(
            "本机正在等待配对，切到「显示配对码」可查看",
            Tone::Info,
        ));
    }
    pair_items.push(if state.pairing_tab == 0 {
        display_block(state)
    } else {
        join_block(state)
    });
    if !state.pairing_status.is_empty() {
        pair_items.push(banner(
            &state.pairing_status,
            if state.pairing_error {
                Tone::Error
            } else {
                Tone::Info
            },
        ));
    }

    let pairing = settings_group(
        "配对",
        Some("两台设备连同一个网络：一台显示 6 位数字，另一台输入即可。全程不经过任何服务器"),
        pair_items,
    );

    let mut peer_items: Vec<Element<'_, Message>> = Vec::new();
    if let Some(error) = &state.device_error {
        let message = format!("设备表读取失败：{error}");
        peer_items.push(banner(message.clone(), Tone::Error));
    }
    if state.device_peers.is_empty() && state.device_error.is_none() {
        peer_items.push(settings_item(
            "暂无已配对设备",
            Some("完成一次配对后，设备会出现在这里；移除后需要重新配对"),
            text("").into(),
        ));
    }
    for peer in &state.device_peers {
        let desc = match &peer.last_addr {
            Some(addr) => format!("{} · 最近地址 {addr}", short_did(&peer.did)),
            None => format!("{} · 尚未连接过", short_did(&peer.did)),
        };
        peer_items.push(settings_item(
            &peer.name,
            Some(desc),
            button("移除")
                .on_press(Message::RemovePeer(peer.did.clone()))
                .padding([6, 12])
                .style(secondary_button)
                .into(),
        ));
    }
    let peers = settings_group(
        "已配对设备",
        Some("剪贴板只在这些设备之间同步；移除后需要重新配对"),
        peer_items,
    );

    let body = column![
        panel_header("设备", "局域网直连的配对与设备管理"),
        identity,
        pairing,
        peers,
        text("连不上时先确认两台设备在同一网络；跨网络请改用 WebDAV 通道。")
            .size(11)
            .color(TEXT_FAINT),
    ]
    .spacing(16);

    container(scrollable(body).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 显示端：未生成时是一行入口，生成后是码 + 倒计时。
fn display_block(state: &UiState) -> Element<'_, Message> {
    let Some(code) = &state.pairing_code else {
        return row![
            column![
                text("由这台设备显示配对码")
                    .size(14)
                    .font(SEMIBOLD)
                    .color(TEXT_MAIN),
                text("生成后 60 秒内，在另一台设备上输入")
                    .size(12)
                    .color(TEXT_MUTED),
            ]
            .spacing(4)
            .width(Length::Fill),
            button("生成配对码")
                .on_press(Message::PairingStartDisplay)
                .padding([9, 16])
                .style(primary_button),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into();
    };

    let remaining = state
        .pairing_handle
        .as_ref()
        .map(|handle| handle.remaining_secs())
        .unwrap_or(0);
    let ratio = state
        .pairing_handle
        .as_ref()
        .map(|handle| handle.remaining_ratio())
        .unwrap_or(0.0);
    let countdown: Element<'_, Message> = if remaining == 0 {
        text("已过期，请重新生成").size(13).color(RED).into()
    } else {
        text(format!("剩余 {remaining} 秒"))
            .size(13)
            .color(TEXT_MUTED)
            .into()
    };

    column![
        text("在另一台设备上输入这组码").size(13).color(TEXT_MUTED),
        text(code).size(34).font(SEMIBOLD).color(ACCENT),
        progress_line(ratio),
        row![
            countdown,
            Space::new().width(Length::Fill),
            button("换一个")
                .on_press(Message::PairingCancel)
                .padding([6, 12])
                .style(secondary_button),
            button("取消")
                .on_press(Message::PairingCancel)
                .padding([6, 12])
                .style(secondary_button),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(10)
    .into()
}

/// 输入端：码输入框 + 配对按钮。
fn join_block(state: &UiState) -> Element<'_, Message> {
    let input = TextInput::new("例如 482 719", &state.pairing_input)
        .on_input(Message::PairingInput)
        .padding([8, 10])
        .style(input_style)
        .width(Length::Fixed(160.0));
    let action: Element<'_, Message> = if state.pairing_busy {
        button("正在查找设备…")
            .padding([9, 16])
            .style(secondary_button)
            .into()
    } else {
        button("配对")
            .on_press(Message::PairingStartJoin)
            .padding([9, 16])
            .style(primary_button)
            .into()
    };

    column![
        text("在这台设备上输入配对码")
            .size(14)
            .font(SEMIBOLD)
            .color(TEXT_MAIN),
        row![input, action].spacing(8).align_y(Alignment::Center),
        text("填另一台设备上显示的 6 位数字，会自动在同一网络下找到对方，最长等 60 秒")
            .size(12)
            .color(TEXT_FAINT),
    ]
    .spacing(8)
    .into()
}

// ---------------------------------------------------------------------------
// 设置页：数据目录 / 高级 HTTP 通道 / 关于
// ---------------------------------------------------------------------------

fn settings_view(state: &UiState) -> Element<'_, Message> {
    let general = settings_group(
        "通用",
        Some("配置文件与历史记录都在这个目录"),
        vec![
            settings_item(
                "数据目录",
                Some(state.config_dir.clone()),
                button("打开")
                    .on_press(Message::OpenConfigDir)
                    .padding([6, 12])
                    .style(secondary_button)
                    .into(),
            ),
            settings_item(
                "同步方式",
                Some("局域网直连 / WebDAV 的通道与参数在「同步通道」页"),
                button("前往")
                    .on_press(Message::SelectPanel(Panel::Sync))
                    .padding([6, 12])
                    .style(secondary_button)
                    .into(),
            ),
            settings_item(
                "开机自启",
                Some("尚未实现（需要各平台分别注册）"),
                text("未支持").size(13).color(TEXT_FAINT).into(),
            ),
        ],
    );

    let body = column![
        panel_header("设置", "数据目录与通用设置"),
        general,
        text(&state.settings_status).size(12).color(TEXT_MUTED),
    ]
    .spacing(16);

    container(scrollable(body).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

// ---------------------------------------------------------------------------
// 关于页
// ---------------------------------------------------------------------------

/// 本软件许可证（来自 workspace.package.license）。
const APP_LICENSE: &str = env!("CARGO_PKG_LICENSE");

fn about_view(_state: &UiState) -> Element<'_, Message> {
    let body = column![
        panel_header("关于", "版本、作者与许可证"),
        about_hero(),
        about_info(),
        about_license(),
    ]
    .spacing(16);

    container(scrollable(body).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 头部：品牌块 + 名称/版本 + 一句话功能。
fn about_hero() -> Element<'static, Message> {
    let logo_tile = container(image(sidebar_logo()).width(44).height(44))
        .padding(12)
        .style(|_| container::Style {
            background: Some(Background::Color(ACCENT_DIM)),
            border: Border {
                color: ACCENT.scale_alpha(0.20),
                width: 1.0,
                radius: border::radius(16.0),
            },
            ..container::Style::default()
        });

    let title = row![
        text("ximed").size(22).font(SEMIBOLD).color(TEXT_MAIN),
        badge(
            format!("v{}", env!("CARGO_PKG_VERSION")),
            ACCENT,
            ACCENT_DIM
        ),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let intro = column![
        title,
        text("输入法数据同步服务 · 局域网直连 / WebDAV / 剪贴板同步 / 托盘常驻")
            .size(13)
            .color(TEXT_MUTED),
    ]
    .spacing(6)
    .width(Length::Fill);

    container(
        row![logo_tile, intro]
            .spacing(16)
            .align_y(Alignment::Center)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding(20)
    .style(card)
    .into()
}

/// 应用信息：只保留「想知道就查」的几项。
fn about_info() -> Element<'static, Message> {
    let facts: Vec<(&'static str, String)> = vec![
        ("作者", "kingzcheung（ximeiorg）".to_string()),
        ("平台", platform_label()),
        ("仓库", "github.com/ximeiorg/ximed".to_string()),
        ("主要依赖", "Iced · Tokio · arboard · xime-p2p".to_string()),
    ];

    let mut body = column![text("应用信息").size(16).font(SEMIBOLD).color(TEXT_MAIN)]
        .spacing(4)
        .width(Length::Fill);
    for (name, value) in facts {
        body = body.push(about_fact(name, value));
    }
    body = body.push(text("完整依赖清单见 Cargo.lock").size(11).color(TEXT_FAINT));

    container(body)
        .width(Length::Fill)
        .padding([16, 18])
        .style(card)
        .into()
}

/// 一行事实：标签左、值右，同一基线。
fn about_fact(name: &'static str, value: String) -> Element<'static, Message> {
    container(
        row![
            text(name).size(13).color(TEXT_MUTED),
            Space::new().width(Length::Fill),
            text(value).size(13).color(TEXT_MAIN),
        ]
        .align_y(Alignment::Center)
        .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding([7, 0])
    .into()
}

/// 许可证 + 技术栈。
fn about_license() -> Element<'static, Message> {
    let body = column![
        text("开源许可").size(16).font(SEMIBOLD).color(TEXT_MAIN),
        row![
            badge(APP_LICENSE, GREEN, GREEN_DIM),
            text("Copyright (C) 2026 kingzcheung")
                .size(12)
                .color(TEXT_MUTED),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
        text("完整文本见仓库 LICENSE；第三方依赖均采用 GPL 兼容许可。")
            .size(13)
            .color(TEXT_MUTED),
        text("技术栈：Rust · Iced · Tokio · arboard · xime-p2p")
            .size(12)
            .color(TEXT_FAINT),
    ]
    .spacing(8)
    .width(Length::Fill);

    container(body)
        .width(Length::Fill)
        .padding(18)
        .style(card)
        .into()
}

/// 运行平台短标识，如 "Linux · x86_64"。
fn platform_label() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    format!("{os} · {}", std::env::consts::ARCH)
}

// ---------------------------------------------------------------------------
// 组件：对齐 libximecore/xime-setup 的组件词汇
// （settings_group / settings_item / tab_bar / badge / banner / switch）
// ---------------------------------------------------------------------------

/// 设置分组卡片：标题 + 可选描述 + 条目。
fn settings_group<'a>(
    title: &'a str,
    description: Option<impl Into<String>>,
    items: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut body = column![text(title).size(16).font(SEMIBOLD).color(TEXT_MAIN)]
        .spacing(8)
        .width(Length::Fill);
    if let Some(desc) = description {
        body = body.push(text(desc.into()).size(13).color(TEXT_MUTED));
    }
    for item in items {
        body = body.push(item);
    }
    container(body)
        .width(Length::Fill)
        .padding(18)
        .style(card)
        .into()
}

/// 设置条目：左侧 label + 描述，右侧控件。
fn settings_item<'a>(
    label: &'a str,
    description: Option<impl Into<String>>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut left = column![text(label).size(14).color(TEXT_MAIN)]
        .spacing(4)
        .width(Length::Fill);
    if let Some(desc) = description {
        left = left.push(text(desc.into()).size(12).color(TEXT_MUTED));
    }
    row![left, control]
        .spacing(12)
        .align_y(Alignment::Center)
        .padding([12, 4])
        .width(Length::Fill)
        .into()
}

/// 开关条目（label + 描述 + 右置开关）。
fn toggle_item<'a>(
    label: &'a str,
    description: Option<&'a str>,
    checked: bool,
    on_toggle: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    settings_item(
        label,
        description.map(|d| d.to_string()),
        switch_control(checked, on_toggle),
    )
}

/// 开关控件：轨道用主色 / 边框色，圆角胶囊（iced 默认配色与自定义主题不同源）。
fn switch_control<'a>(on: bool, on_toggle: impl Fn(bool) -> Message + 'a) -> Element<'a, Message> {
    toggler(on)
        .on_toggle(on_toggle)
        .style(move |_theme, _status| iced::widget::toggler::Style {
            background: Background::Color(if on { ACCENT } else { BORDER_STRONG }),
            background_border_width: 0.0,
            background_border_color: Color::TRANSPARENT,
            foreground: Background::Color(Color::WHITE),
            foreground_border_width: 0.0,
            foreground_border_color: Color::TRANSPARENT,
            text_color: Some(TEXT_MAIN),
            border_radius: Some(border::radius(999.0)),
            padding_ratio: 0.15,
        })
        .into()
}

/// 分段控件（tab bar）：等宽标签 + 药丸选中项，轨道比页面深一档。
fn tab_bar<'a>(
    labels: &[&'a str],
    active: usize,
    on_select: impl Fn(usize) -> Message + 'a,
) -> Element<'a, Message> {
    let mut bar = row![].spacing(2).padding(3);
    let cell = labels
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(1) as f32
        * 14.0
        + 32.0;

    for (index, label) in labels.iter().enumerate() {
        let is_active = index == active;
        let label = *label;
        bar = bar.push(
            button(
                container(
                    text(label)
                        .size(13)
                        .color(if is_active { ACCENT } else { TEXT_MUTED }),
                )
                .height(Length::Fill)
                .center_y(Length::Fill),
            )
            .padding([0, 12])
            .width(Length::Fixed(cell))
            .height(Length::Fixed(CONTROL_HEIGHT - 6.0))
            .style(move |_theme, status| {
                let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                let background = if is_active {
                    Some(Background::Color(TAB_ACTIVE))
                } else if hovered {
                    Some(Background::Color(SURFACE_HOVER))
                } else {
                    None
                };
                button::Style {
                    background,
                    text_color: if is_active { ACCENT } else { TEXT_MUTED },
                    border: Border {
                        color: Color::TRANSPARENT,
                        width: 0.0,
                        radius: border::radius(RADIUS_SM),
                    },
                    ..button::Style::default()
                }
            })
            .on_press(on_select(index)),
        );
    }

    container(bar)
        .width(Length::Shrink)
        .style(|_| container::Style {
            background: Some(Background::Color(TRACK)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(RADIUS_MD),
            },
            ..container::Style::default()
        })
        .into()
}

/// 状态胶囊。
fn badge<'a>(label: impl Into<String>, fg: Color, bg: Color) -> Element<'a, Message> {
    container(text(label.into()).size(12).color(fg))
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

/// 提示条色调。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Info,
    Error,
}

/// 提示条：进行中用主色，失败用错误色。不抢标题的视觉权重。
fn banner<'a>(message: impl Into<String>, tone: Tone) -> Element<'a, Message> {
    let accent = match tone {
        Tone::Info => ACCENT,
        Tone::Error => RED,
    };
    container(text(message.into()).size(13).color(accent))
        .width(Length::Fill)
        .padding([10, 14])
        .style(move |_| container::Style {
            background: Some(Background::Color(accent.scale_alpha(0.10))),
            border: Border {
                color: accent.scale_alpha(0.35),
                width: 1.0,
                radius: border::radius(RADIUS_SM),
            },
            ..container::Style::default()
        })
        .into()
}

/// 细进度条（配对码倒计时）：卡片里的一行信息，不抢标题的视觉权重。
fn progress_line(value: f32) -> Element<'static, Message> {
    iced::widget::progress_bar(0.0..=1.0, value.clamp(0.0, 1.0))
        .length(Length::Fill)
        .girth(Length::Fixed(6.0))
        .style(|_theme| iced::widget::progress_bar::Style {
            background: Background::Color(SURFACE_VARIANT),
            bar: Background::Color(ACCENT),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(999.0),
            },
        })
        .into()
}

/// 通道 id → 展示名。
fn channel_label(id: &str) -> String {
    client::channel::Channel::from_id(id)
        .map(|channel| channel.display_name().to_string())
        .unwrap_or_else(|| id.to_string())
}

/// 设备标识短码（前 8 位）：完整 did 是 32 位 hex，界面上一行放不下。
fn short_did(did: &str) -> String {
    did.chars().take(8).collect()
}

/// 行内文本输入（宽度固定，配合 settings_item 右置）。
fn text_control<'a>(
    value: &'a str,
    placeholder: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    TextInput::new(placeholder, value)
        .on_input(on_input)
        .padding([8, 10])
        .style(input_style)
        .width(Length::Fixed(300.0))
        .into()
}

/// 行内密码输入。
fn password_control<'a>(
    value: &'a str,
    placeholder: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    TextInput::new(placeholder, value)
        .secure(true)
        .on_input(on_input)
        .padding([8, 10])
        .style(input_style)
        .width(Length::Fixed(300.0))
        .into()
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

    #[test]
    fn app_license_is_gpl() {
        assert_eq!(APP_LICENSE, "GPL-3.0-or-later");
    }

    #[test]
    fn sidebar_logo_handle_is_cached() {
        // Handle 的 Id 相同，渲染器才会复用已上传的纹理；每次 view 新建会闪。
        assert_eq!(sidebar_logo(), sidebar_logo());
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
