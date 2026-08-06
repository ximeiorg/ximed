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
use iced::widget::{
    button, column, container, rule, row, scrollable, text,
};
use iced::{
    Alignment, Background, Border, Color, Element, Length, Subscription, Task, Theme, border,
};

/// 面板切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Status,
    History,
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
    /// 显示窗口（托盘菜单/左键点击触发）。
    ShowWindow,
    /// 隐藏窗口到托盘（托盘菜单触发）。
    HideWindow,
    /// 请求退出（托盘菜单）。
    Quit,
}

/// UI → 后台的桌面级命令（跨线程 channel 负载）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCmd {
    /// 启动本地 server。
    ServerStart,
    /// 重启本地 server（先停后启）。
    ServerRestart,
    /// 停止本地 server。
    ServerStop,
    /// 打开配置文件所在目录。
    OpenConfigDir,
}

/// UI 消息（iced Message：channel 事件 + 按钮动作）。
#[derive(Debug, Clone)]
pub enum Message {
    /// 来自后台线程的状态事件。
    Backend(UiMsg),
    /// 窗口已打开（记录 window id，供显示/隐藏控制）。
    WindowOpened(iced::window::Id),
    /// 窗口关闭请求（点红叉 → 隐藏到托盘而非退出）。
    WindowCloseRequested(iced::window::Id),
    /// 切换到某个面板。
    SelectPanel(Panel),
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
    /// 是否显示服务器控制区（desktop 模式 true，client-only false）。
    pub local_server: bool,
    /// 配置目录路径（打开文件管理器用）。
    pub config_dir: String,
    /// 后台命令发送端。
    cmd_tx: UiCmdSender,
    /// 后台事件发送端（托盘事件也经此注入）。
    tx: UiSender,
    /// 主窗口 id（首次打开时记录，供显示/隐藏控制）。
    window_id: Option<iced::window::Id>,
    /// 托盘是否已创建（首个 Backend 消息时创建，主线程）。
    tray_started: bool,
}

impl UiState {
    pub fn new(
        cfg: &ClientConfig,
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
            local_server,
            config_dir,
            cmd_tx,
            tx,
            window_id: None,
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
    tx: UiSender,
    cmd_tx: UiCmdSender,
    rx: std::sync::mpsc::Receiver<UiMsg>,
    local_server: bool,
    config_dir: String,
) -> iced::Result {
    let state = UiState::new(&cfg, tx, cmd_tx, local_server, config_dir);
    let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
    iced::application(
        move || (state.clone(), Task::none()),
        update,
        view,
    )
    .title("ximed")
    .exit_on_close_request(false)
    .subscription(move |_state| {
        Subscription::batch([
            backend_events(rx.clone()),
            window_events(),
        ])
    })
    .theme(theme)
    .run()
}

/// 窗口事件订阅：记录窗口 id（打开时）、拦截关闭请求（隐藏到托盘）。
fn window_events() -> Subscription<Message> {
    Subscription::batch([
        iced::window::open_events().map(Message::WindowOpened),
        iced::window::close_requests().map(Message::WindowCloseRequested),
    ])
}

/// 固定主题（状态无关）。
fn theme(_state: &UiState) -> iced::Theme {
    let mut palette = iced::theme::Palette::TOKYO_NIGHT;
    palette.background = Color::from_rgb8(0x12, 0x13, 0x1c);
    palette.text = Color::from_rgb8(0xcd, 0xd6, 0xf4);
    palette.primary = Color::from_rgb8(0x7a, 0xa2, 0xf7);
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
        Message::WindowCloseRequested(id) => {
            state.window_id = Some(id);
            // 关闭按钮：隐藏到托盘而非退出
            return hide_window(state);
        }
        Message::SelectPanel(panel) => {
            state.panel = panel;
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
// 现代化样式：大圆角卡片、柔和配色
// ---------------------------------------------------------------------------

/// 主题颜色常量。
const SURFACE: Color = Color::from_rgb8(0x1a, 0x1b, 0x26); // 卡片底色
const SURFACE_ALT: Color = Color::from_rgb8(0x16, 0x17, 0x22); // 次级面板
const TEXT_MUTED: Color = Color::from_rgb8(0x8b, 0x93, 0xad);
const TEXT_MAIN: Color = Color::from_rgb8(0xcd, 0xd6, 0xf4);
const ACCENT: Color = Color::from_rgb8(0x7a, 0xa2, 0xf7); // 主蓝
const ACCENT_DIM: Color = Color::from_rgb8(0x35, 0x3b, 0x50);
const GREEN: Color = Color::from_rgb8(0x9e, 0xce, 0x6a);
const RED: Color = Color::from_rgb8(0xf7, 0x76, 0x8e);
const BORDER: Color = Color::from_rgb8(0x2a, 0x2d, 0x3e);

const RADIUS: f32 = 14.0;

/// 卡片容器样式：大圆角 + 边框。
fn card(theme: &Theme) -> container::Style {
    let bg = match theme {
        Theme::Custom(_) => SURFACE,
        _ => Color::from_rgb8(0x1a, 0x1b, 0x26),
    };
    container::Style {
        background: Some(Background::Color(bg)),
        text_color: Some(TEXT_MAIN),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::radius(RADIUS),
        },
        ..container::Style::default()
    }
}

/// 次级面板：更深的底色，用于侧边栏等次级区域。
fn surface_alt(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE_ALT)),
        text_color: Some(TEXT_MAIN),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::radius(RADIUS),
        },
        ..container::Style::default()
    }
}

/// 导航按钮样式（由 view 的 [`nav_button`] 使用）。
fn nav_style(theme: &Theme, _status: button::Status, active: bool) -> button::Style {
    let base = button::Style {
        background: None,
        text_color: TEXT_MUTED,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(10.0),
        },
        ..button::Style::default()
    };
    if active {
        button::Style {
            background: Some(Background::Color(ACCENT_DIM)),
            text_color: ACCENT,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(10.0),
            },
            ..base
        }
    } else {
        let _ = theme;
        base
    }
}

/// 主要操作按钮（开启/保存）：主色填充 + 圆角。
fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let mut style = button::Style {
        background: Some(Background::Color(ACCENT)),
        text_color: Color::from_rgb8(0x0b, 0x0f, 0x1c),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(10.0),
        },
        ..button::Style::default()
    };
    match status {
        button::Status::Hovered => {
            style.background = Some(Background::Color(Color::from_rgb8(0x8b, 0xb0, 0xfa)));
        }
        button::Status::Pressed => {
            style.background = Some(Background::Color(Color::from_rgb8(0x66, 0x8d, 0xe3)));
        }
        _ => {}
    }
    style
}

/// 停止按钮：红色填充 + 圆角。
fn stop_button(theme: &Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let mut style = button::Style {
        background: Some(Background::Color(RED)),
        text_color: Color::from_rgb8(0x1a, 0x1b, 0x26),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(10.0),
        },
        ..button::Style::default()
    };
    match status {
        button::Status::Hovered => {
            style.background = Some(Background::Color(Color::from_rgb8(0xff, 0x8f, 0xa3)));
        }
        _ => {}
    }
    style
}

/// 次要操作按钮：边框 + 圆角。
fn secondary_button(theme: &Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let mut style = button::Style {
        background: None,
        text_color: TEXT_MAIN,
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::radius(10.0),
        },
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(Background::Color(ACCENT_DIM));
    }
    style
}

/// 危险按钮（退出）：透明底 + 红色文字 + 悬停红边。
fn danger_button(theme: &Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let mut style = button::Style {
        background: None,
        text_color: RED,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::radius(10.0),
        },
        ..button::Style::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(Background::Color(Color { a: 0.12, ..RED }));
    }
    style
}

// ---------------------------------------------------------------------------
// 视图
// ---------------------------------------------------------------------------

fn view(state: &UiState) -> Element<'_, Message> {
    // 左分栏：面板导航
    let nav = column![
        text("ximed").size(20).color(TEXT_MAIN).font(iced::Font::DEFAULT),
        text("剪贴板同步").size(11).color(TEXT_MUTED),
        rule::horizontal(1),
        nav_button("状态", Panel::Status, state.panel),
        nav_button("剪贴板历史", Panel::History, state.panel),
        rule::horizontal(1),
        button(text("退出").color(RED))
            .on_press(Message::Quit)
            .width(Length::Fill)
            .style(danger_button),
    ]
    .spacing(6)
    .padding(12);

    let sidebar = container(nav)
        .width(160)
        .height(Length::Fill)
        .padding(10)
        .style(surface_alt);

    // 右分栏：按面板渲染内容
    let content = match state.panel {
        Panel::Status => status_view(state),
        Panel::History => history_view(state),
    };
    let content = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16)
        .style(card);

    row![sidebar, content]
        .spacing(12)
        .padding(12)
        .into()
}

fn nav_button(label: &str, panel: Panel, active: Panel) -> Element<'_, Message> {
    let is_active = panel == active;
    button(
        row![
            dot(is_active),
            text(label).color(if is_active { ACCENT } else { TEXT_MUTED }),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .on_press(Message::SelectPanel(panel))
    .width(Length::Fill)
    .padding(10)
    .style(move |theme, status| nav_style(theme, status, is_active))
    .into()
}

/// 导航项左侧的小圆点指示器。
fn dot(active: bool) -> Element<'static, Message> {
    let color = if active { ACCENT } else { Color::TRANSPARENT };
    container(text("").width(8).height(8))
        .width(8)
        .height(8)
        .style(move |_| container::Style {
            background: Some(Background::Color(color)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::radius(4.0),
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
            text("状态").size(20).color(TEXT_MAIN),
            text("服务与同步引擎运行情况").size(12).color(TEXT_MUTED),
            rule::horizontal(1),
            stat_row("服务器", server_value, server_up),
            stat_row("设备", state.source.clone(), true),
            stat_row("同步引擎", state.engine_status.clone(), engine_up),
        ]
        .spacing(10)
        .padding(12),
    )
    .width(Length::Fill)
    .style(card);

    let mut content = column![info_card].spacing(12).padding(8);

    // 服务器控制区：仅本地 server（desktop 形态）显示
    if state.local_server {
        let controls = server_controls(state);
        content = content.push(controls);
    }
    content = content.push(text(&state.config_dir).size(11).color(TEXT_MUTED));
    scrollable(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 本地服务器控制卡片：开启 / 重启 / 关闭 + 打开配置文件目录。
fn server_controls(state: &UiState) -> Element<'static, Message> {
    let title = if state.server_running {
        "服务器运行中"
    } else {
        "服务器已停止"
    };
    let status_dot = stat_dot(state.server_running);

    let mut btns = row![].spacing(8).align_y(Alignment::Center);
    if state.server_running {
        btns = btns.push(
            button("重启").on_press(Message::ServerRestart).padding(10).style(secondary_button),
        );
        btns = btns.push(button("关闭").on_press(Message::ServerStop).padding(10).style(stop_button));
    } else {
        btns = btns.push(button("开启").on_press(Message::ServerStart).padding(10).style(primary_button));
    }

    container(
        column![
            row![status_dot, text(title).size(16).color(TEXT_MAIN)]
                .spacing(8)
                .align_y(Alignment::Center),
            text("本地服务器默认不随应用自动启动，请手动开启。")
                .size(12)
                .color(TEXT_MUTED),
            btns,
            button("打开配置文件所在目录")
                .on_press(Message::OpenConfigDir)
                .padding(8)
                .style(secondary_button),
        ]
        .spacing(10)
        .padding(12),
    )
    .width(Length::Fill)
    .style(card)
    .into()
}

/// 状态行：彩色圆点 + 标签 + 值。
fn stat_row(label: &'static str, value: String, ok: bool) -> Element<'static, Message> {
    row![
        stat_dot(ok),
        text(label).size(14).color(TEXT_MUTED).width(Length::Fixed(80.0)),
        text(value).size(14).color(TEXT_MAIN),
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .into()
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

fn history_view(state: &UiState) -> Element<'_, Message> {
    let header = row![
        text("seq").size(12).color(TEXT_MUTED).width(Length::Fixed(48.0)),
        text("type").size(12).color(TEXT_MUTED).width(Length::Fixed(72.0)),
        text("source").size(12).color(TEXT_MUTED).width(Length::Fixed(96.0)),
        text("content").size(12).color(TEXT_MUTED).width(Length::Fill),
    ]
    .spacing(8)
    .padding(4);

    let mut rows = column![
        text("剪贴板历史").size(20).color(TEXT_MAIN),
        text("最近同步的剪贴板内容").size(12).color(TEXT_MUTED),
        rule::horizontal(1),
        header,
    ]
    .spacing(8)
    .padding(4);

    if state.history.is_empty() {
        rows = rows.push(
            container(
                text("（暂无历史记录，或服务器未启用 history）")
                    .size(13)
                    .color(TEXT_MUTED),
            )
            .padding(12),
        );
    } else {
        for item in &state.history {
            let preview: String = item.text.chars().take(60).collect();
            let row_item = row![
                text(item.seq.to_string())
                    .size(13)
                    .color(TEXT_MUTED)
                    .width(Length::Fixed(48.0)),
                text(&item.kind).size(13).color(TEXT_MAIN).width(Length::Fixed(72.0)),
                text(item.source.as_deref().unwrap_or("-"))
                    .size(13)
                    .color(TEXT_MAIN)
                    .width(Length::Fixed(96.0)),
                text(preview).size(13).color(TEXT_MAIN).width(Length::Fill),
            ]
            .spacing(8)
            .padding(6);

            let card_row = container(row_item)
                .width(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(SURFACE_ALT)),
                    border: Border {
                        color: BORDER,
                        width: 1.0,
                        radius: border::radius(10.0),
                    },
                    ..container::Style::default()
                });
            rows = rows.push(card_row);
        }
    }
    scrollable(rows)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// 订阅身份 + 数据：包裹接收端，实现 `Hash + Clone` 供 `Subscription::run_with`。
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
fn backend_events(rx: std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<UiMsg>>>) -> Subscription<Message> {
    use iced::futures::channel::mpsc as fmpsc;
    Subscription::run_with(RxId(rx), |rx| {
        let rx = rx.clone();
        iced::stream::channel(
            64,
            move |mut tx: fmpsc::Sender<Message>| async move {
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
            },
        )
    })
}
