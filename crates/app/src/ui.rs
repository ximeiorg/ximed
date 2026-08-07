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
    Space, TextInput, button, column, container, image, row, scrollable, text, text_input,
};
use iced::{
    Alignment, Background, Border, Color, Element, Font, Length, Subscription, Task, Theme, border,
};

use crate::settings::{AppSettings, SyncMode};

/// 面板切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Status,
    History,
    Settings,
}

/// 后台 → UI 的桌面级事件（跨线程 channel 负载）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiMsg {
    /// 同步引擎已启动（携带来源设备）。
    EngineStarted { source: String },
    /// 同步引擎已停止。
    EngineStopped { source: String },
    /// 服务器已监听。
    ServerListening { addr: String },
    /// 服务器已停止。
    ServerStopped { addr: String },
    /// 剪贴板历史刷新。
    History { items: Vec<HistoryItem> },
    /// client 连接状态上报（引擎 push/拉取结果）。
    SyncStatus { online: bool },
    /// 设置已保存并应用（连接模式/远程服务器已生效）。
    SettingsApplied,
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
    /// 应用新设置（连接模式 + 远程服务器），持久化并重启 client 引擎。
    ApplySettings {
        mode: SyncMode,
        remote_url: String,
        remote_username: String,
        remote_password: String,
    },
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
    /// 切换连接模式（本地 / 远程 server）。
    SettingsMode(SyncMode),
    /// 远程服务器地址输入。
    SettingsUrl(String),
    /// 远程服务器用户名输入。
    SettingsUsername(String),
    /// 远程服务器密码输入。
    SettingsPassword(String),
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
    /// 远程服务器地址（设置面板编辑）。
    pub remote_url: String,
    /// 远程服务器用户名（设置面板编辑）。
    pub remote_username: String,
    /// 远程服务器密码（设置面板编辑）。
    pub remote_password: String,
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
            remote_url: app_settings.remote_url.clone(),
            remote_username: app_settings.remote_username.clone(),
            remote_password: app_settings.remote_password.clone().unwrap_or_default(),
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
        Message::Backend(UiMsg::ServerListening { addr }) => {
            state.server_status = format!("listening on {addr}");
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
        Message::Backend(UiMsg::SettingsApplied) => {
            state.settings_status = "已保存并应用。".to_string();
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
        Message::SettingsMode(mode) => {
            state.sync_mode = mode;
            state.settings_status = String::new();
        }
        Message::SettingsUrl(url) => {
            state.remote_url = url;
            state.settings_status = String::new();
        }
        Message::SettingsUsername(username) => {
            state.remote_username = username;
            state.settings_status = String::new();
        }
        Message::SettingsPassword(password) => {
            state.remote_password = password;
            state.settings_status = String::new();
        }
        Message::ApplySettings => {
            let cmd = UiCmd::ApplySettings {
                mode: state.sync_mode,
                remote_url: state.remote_url.clone(),
                remote_username: state.remote_username.clone(),
                remote_password: state.remote_password.clone(),
            };
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

    // 设置面板仅 desktop 形态（存在本地 server 可切换）显示
    if state.local_server {
        nav = nav.push(nav_button("设置", Panel::Settings, state.panel));
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
        Panel::Settings => settings_view(state),
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
                    text("或服务器未启用 history 功能")
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
            col = col.push(history_card(item, expanded, state.window_width));
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
fn history_card(item: &HistoryItem, expanded: bool, window_width: f32) -> Element<'_, Message> {
    // 仅当内容超过 3 行时才允许展开：按内容区宽度估算换行行数。
    let has_more = history_has_more(&item.text, window_width);

    // 内容区：收起时固定高度 + 裁剪（最多 3 行）；展开时全文（限高滚动，隐藏内部滚动条避免浮在文字上）
    let content = if expanded {
        Element::from(
            container(
                scrollable(text(&item.text).size(13).color(TEXT_MAIN))
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
            container(text(&item.text).size(13).color(TEXT_MAIN))
                .width(Length::Fill)
                .height(Length::Fixed(HISTORY_CONTENT_HEIGHT))
                .clip(true),
        )
    };

    // 底部 tag 行：类型 / 来源 / 序号 / 时间
    let source = item.source.clone().unwrap_or_else(|| "unknown".to_string());
    let time = format_time(item.created_at);
    let tags = row![
        history_tag(item.kind.clone()),
        history_tag(source),
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

/// 设置面板：连接模式（本地 / 远程 server）+ 远程服务器表单 + 保存。
fn settings_view(state: &UiState) -> Element<'_, Message> {
    let mode_local = seg_button("本地服务器", SyncMode::Local, state.sync_mode);
    let mode_remote = seg_button("远程服务器", SyncMode::Remote, state.sync_mode);
    let segmented =
        container(row![mode_local, mode_remote].spacing(2).padding(2)).style(|_theme| {
            container::Style {
                background: Some(Background::Color(SURFACE_HOVER)),
                border: Border {
                    color: Color::TRANSPARENT,
                    width: 0.0,
                    radius: border::radius(RADIUS_MD),
                },
                ..container::Style::default()
            }
        });

    let hint = match state.sync_mode {
        SyncMode::Local => text("client 自动连接本机 server，地址/端口跟随 server 配置。")
            .size(12)
            .color(TEXT_MUTED),
        SyncMode::Remote => text("client 连接下方配置的远程 server；切换后本机 server 将停止。")
            .size(12)
            .color(TEXT_MUTED),
    };

    let mut body = column![card_title("连接模式"), segmented, hint,].spacing(10);

    if state.sync_mode == SyncMode::Remote {
        body = body.push(
            column![
                Space::new().height(6),
                field_label("远程服务器地址"),
                TextInput::new("https://example.com:8443", &state.remote_url)
                    .on_input(Message::SettingsUrl)
                    .padding(10)
                    .style(input_style),
                Space::new().height(2),
                field_label("用户名"),
                TextInput::new("alice", &state.remote_username)
                    .on_input(Message::SettingsUsername)
                    .padding(10)
                    .style(input_style),
                Space::new().height(2),
                field_label("密码"),
                TextInput::new("", &state.remote_password)
                    .secure(true)
                    .on_input(Message::SettingsPassword)
                    .padding(10)
                    .style(input_style),
            ]
            .spacing(6),
        );
    }

    body = body.push(Space::new().height(4));
    body = body.push(
        button("保存并应用")
            .on_press(Message::ApplySettings)
            .padding([10, 18])
            .style(primary_button),
    );
    body = body.push(text(&state.settings_status).size(12).color(TEXT_MUTED));

    container(
        column![
            panel_header("设置", "连接模式与远程服务器"),
            Space::new().height(8),
            container(body).width(Length::Fill).padding(16).style(card),
        ]
        .spacing(10),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// 设置表单字段标签。
fn field_label<'a>(label: &'a str) -> Element<'a, Message> {
    text(label).size(12).color(TEXT_MUTED).into()
}

/// 分段选择按钮（Tailwind 风格的 segmented control）。
fn seg_button<'a>(label: &'a str, mode: SyncMode, current: SyncMode) -> Element<'a, Message> {
    let active = mode == current;
    button(
        text(label)
            .size(13)
            .color(if active { TEXT_MAIN } else { TEXT_MUTED }),
    )
    .on_press(Message::SettingsMode(mode))
    .width(Length::Fill)
    .padding([8, 14])
    .style(move |_theme, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: if active || hovered {
                Some(Background::Color(SURFACE))
            } else {
                None
            },
            text_color: if active { TEXT_MAIN } else { TEXT_MUTED },
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(RADIUS_SM),
            },
            ..button::Style::default()
        }
    })
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
