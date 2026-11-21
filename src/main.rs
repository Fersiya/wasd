#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod tgproxy;
mod update;

use engine::{Server, Settings, APP};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Child,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    window::{Icon as WinIcon, Window, WindowBuilder},
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use wry::{WebContext, WebView, WebViewBuilder};

/// Порт для проверки «уже запущено»: второй запуск просит первый показать окно.
const SINGLE_PORT: u16 = 47631;
/// Как часто проверять обновления.
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 3600);

enum UserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    Ui(String),
    Show,
    /// (процесс ядра, имя сервера, ключ сервера)
    Started(Result<(Child, String, String), String>),
    Servers(Result<Vec<Server>, String>),
    UpdateChecked {
        manual: bool,
        result: Result<Option<update::Release>, String>,
    },
    /// Ok = версия, до которой обновились
    UpdateInstalled(Result<String, String>),
}

// ───────────────────────── состояние ─────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum St {
    Off,
    Connecting,
    On,
    Error,
}

impl St {
    fn key(self) -> &'static str {
        match self {
            St::Off => "off",
            St::Connecting => "connecting",
            St::On => "on",
            St::Error => "error",
        }
    }
    fn text(self) -> &'static str {
        match self {
            St::Off => "Отключено",
            St::Connecting => "Подключение…",
            St::On => "Подключено",
            St::Error => "Ошибка",
        }
    }
}

struct App {
    settings: Settings,
    st: St,
    server_name: String,
    server_key: String,
    error: String,
    servers: Vec<Server>,
    refreshing: bool,
    autostart: bool,
    child: Option<Child>,
    update: Option<update::Release>,
    update_busy: bool,
    update_failed: bool,
    update_note: String,
    tg: Option<tgproxy::Proxy>,
    tg_error: String,
}

impl App {
    fn sub(&self) -> String {
        match self.st {
            St::On => self.server_name.clone(),
            St::Connecting => "Подождите…".into(),
            St::Error => "Не удалось подключиться".into(),
            St::Off if self.settings.link.trim().is_empty() => {
                "Добавь ссылку на подписку ниже".into()
            }
            St::Off => "Нажми на кнопку, чтобы подключиться".into(),
        }
    }

    fn json(&self) -> String {
        json!({
            "status": self.st.key(),
            "status_text": self.st.text(),
            "sub": self.sub(),
            "error": self.error,
            "servers": self.servers.iter()
                .map(|s| json!({ "key": s.key(), "name": s.name, "ping": s.ping }))
                .collect::<Vec<_>>(),
            "refreshing": self.refreshing,
            "selected": self.settings.server,
            "connected_key": if self.st == St::On { self.server_key.clone() } else { String::new() },
            "bypass_ru": self.settings.bypass_ru,
            "autostart": self.autostart,
            "autoconnect": self.settings.autoconnect,
            "auto_update": self.settings.auto_update,
            "verbose": self.settings.verbose,
            "tg_proxy": self.tg.is_some(),
            "tg_error": self.tg_error,
            "tg_link": if self.tg.is_some() { tgproxy::link(&self.settings.tg_secret) } else { String::new() },
            "link": self.settings.link,
            "update": self.update.as_ref()
                .map(|r| json!({ "version": r.version, "busy": self.update_busy })),
            "update_note": self.update_note,
            "version": env!("CARGO_PKG_VERSION"),
        })
        .to_string()
    }
}

// ─────────────────────────── окно ───────────────────────────

struct Win {
    // порядок полей важен: сначала закрывается webview, потом контекст и окно
    webview: WebView,
    _ctx: WebContext,
    window: Window,
}

fn window_icon() -> Option<WinIcon> {
    WinIcon::from_rgba(include_bytes!("../res/window.rgba").to_vec(), 64, 64).ok()
}

fn tray_icon_for(st: St) -> Icon {
    let bytes: &[u8] = match st {
        St::On => include_bytes!("../res/tray_on.rgba"),
        St::Error => include_bytes!("../res/tray_err.rgba"),
        _ => include_bytes!("../res/tray_off.rgba"),
    };
    Icon::from_rgba(bytes.to_vec(), 32, 32).expect("иконка трея")
}

fn open_window(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: &EventLoopProxy<UserEvent>,
) -> Option<Win> {
    let window = WindowBuilder::new()
        .with_title(APP)
        .with_inner_size(LogicalSize::new(440.0, 720.0))
        .with_min_inner_size(LogicalSize::new(380.0, 460.0))
        .with_resizable(true)
        .with_maximizable(true)
        .with_window_icon(window_icon())
        .build(target)
        .ok()?;
    let mut ctx = WebContext::new(Some(engine::data_dir().join("webview")));
    let p = proxy.clone();
    let webview = WebViewBuilder::with_web_context(&mut ctx)
        .with_html(include_str!("../ui/index.html"))
        .with_ipc_handler(move |req| {
            let _ = p.send_event(UserEvent::Ui(req.body().to_string()));
        })
        .build(&window)
        .ok()?;
    Some(Win {
        webview,
        _ctx: ctx,
        window,
    })
}

fn show_window(
    win: &mut Option<Win>,
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: &EventLoopProxy<UserEvent>,
) {
    if let Some(w) = win.as_ref() {
        w.window.set_minimized(false);
        w.window.set_visible(true);
        w.window.set_focus();
    } else {
        *win = open_window(target, proxy);
    }
}

/// Обновляет иконку и меню в трее и перерисовывает окно (если открыто).
fn sync(app: &App, tray: &Option<TrayIcon>, status: &MenuItem, toggle: &MenuItem, win: &Option<Win>) {
    let text = if app.st == St::On {
        format!("{}: {}", app.st.text(), app.server_name)
    } else {
        app.st.text().to_string()
    };
    status.set_text(&text);
    toggle.set_text(if app.child.is_some() { "Отключить" } else { "Подключить" });
    if let Some(t) = tray {
        let _ = t.set_icon(Some(tray_icon_for(app.st)));
        let _ = t.set_tooltip(Some(format!("{APP}: {text}")));
    }
    if let Some(w) = win {
        let _ = w.webview.evaluate_script(&format!("window.__state({})", app.json()));
    }
}

fn push_log(win: &Option<Win>) {
    if let Some(w) = win {
        let text = engine::log_tail(9000);
        let js = serde_json::to_string(&text).unwrap_or_else(|_| "\"\"".into());
        let _ = w.webview.evaluate_script(&format!("window.__log({js})"));
    }
}

// ───────────────────────── действия ─────────────────────────

fn notify(title: &str, body: &str) {
    #[cfg(windows)]
    {
        let (t, b) = (title.to_string(), body.to_string());
        std::thread::spawn(move || {
            let _ = notify_rust::Notification::new().summary(&t).body(&b).show();
        });
    }
    #[cfg(not(windows))]
    {
        let _ = (title, body);
    }
}

/// Включает или выключает Telegram-прокси согласно настройке.
fn apply_tg(app: &mut App) {
    app.tg = None; // Drop останавливает приём подключений
    app.tg_error.clear();
    if !app.settings.tg_proxy {
        return;
    }
    if app.settings.tg_secret.len() != 32 {
        app.settings.tg_secret = tgproxy::new_secret();
        engine::save_settings(&app.settings);
    }
    // порт освобождается не мгновенно: пробуем несколько раз
    let mut last = String::new();
    for _ in 0..10 {
        match tgproxy::Proxy::start(&app.settings.tg_secret) {
            Ok(p) => {
                app.tg = Some(p);
                return;
            }
            Err(e) => last = e,
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    app.settings.tg_proxy = false;
    engine::save_settings(&app.settings);
    app.tg_error = last;
}

fn begin_connect(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    app.st = St::Connecting;
    app.error.clear();
    let p = proxy.clone();
    let s = app.settings.clone();
    std::thread::spawn(move || {
        let _ = p.send_event(UserEvent::Started(engine::start_core(&s)));
    });
}

/// Скачивает список серверов из подписки и меряет пинг.
fn refresh_servers(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    let link = app.settings.link.clone();
    if link.trim().is_empty() {
        return;
    }
    app.refreshing = true;
    let p = proxy.clone();
    std::thread::spawn(move || {
        let res = engine::load_servers(&link).map(|mut list| {
            engine::ping_all(&mut list);
            list
        });
        let _ = p.send_event(UserEvent::Servers(res));
    });
}

fn restart(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    engine::stop_core(&mut app.child);
    if app.st != St::Connecting {
        begin_connect(proxy, app);
    }
}

fn toggle_connection(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    if app.child.is_some() {
        engine::stop_core(&mut app.child);
        app.st = St::Off;
        app.error.clear();
        maybe_auto_update(proxy, app);
    } else if app.st != St::Connecting {
        if app.settings.link.trim().is_empty() {
            app.st = St::Error;
            app.error = "Сначала добавь ссылку или подписку в поле внизу окна.".into();
        } else {
            begin_connect(proxy, app);
        }
    }
}

fn spawn_update_check(proxy: &EventLoopProxy<UserEvent>, manual: bool) {
    let p = proxy.clone();
    std::thread::spawn(move || {
        let _ = p.send_event(UserEvent::UpdateChecked {
            manual,
            result: update::check(),
        });
    });
}

fn start_update_install(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    if app.update_busy {
        return;
    }
    if let Some(rel) = app.update.clone() {
        app.update_busy = true;
        app.update_note.clear();
        let p = proxy.clone();
        std::thread::spawn(move || {
            let res = update::install(&rel).map(|_| rel.version.clone());
            let _ = p.send_event(UserEvent::UpdateInstalled(res));
        });
    }
}

/// Автоустановка, пока VPN не подключён (перезапуск не должен рвать соединение).
fn maybe_auto_update(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    let idle = app.st == St::Off || app.st == St::Error;
    if app.settings.auto_update && idle && app.update.is_some() && !app.update_failed {
        start_update_install(proxy, app);
    }
}

/// Если Nyxar уже запущен, просим его показать окно и выходим.
fn single_instance(proxy: &EventLoopProxy<UserEvent>) {
    if let Ok(mut s) = TcpStream::connect(("127.0.0.1", SINGLE_PORT)) {
        let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
        let _ = s.write_all(b"show");
        let mut buf = [0u8; 5];
        if s.read_exact(&mut buf).is_ok() && &buf == b"nyxar" {
            std::process::exit(0);
        }
    }
    if let Ok(listener) = TcpListener::bind(("127.0.0.1", SINGLE_PORT)) {
        let p = proxy.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut c = conn;
                let _ = c.write_all(b"nyxar");
                let _ = p.send_event(UserEvent::Show);
            }
        });
    }
}

// ──────────────────────────── main ────────────────────────────

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    single_instance(&proxy);

    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Menu(e));
        }));
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Tray(e));
        }));
    }

    let minimized = std::env::args().any(|a| a == "--minimized");

    let status_item = MenuItem::new("Отключено", false, None);
    let open_item = MenuItem::new("Открыть окно", true, None);
    let toggle_item = MenuItem::new("Подключить", true, None);
    let quit_item = MenuItem::new("Выход", true, None);
    let menu = Menu::new();
    let _ = menu.append(&status_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&open_item);
    let _ = menu.append(&toggle_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit_item);

    let mut tray = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip(format!("{APP}: отключено"))
            .with_icon(tray_icon_for(St::Off))
            .build()
            .expect("не удалось создать иконку в трее"),
    );

    let mut app = App {
        settings: engine::load_settings(),
        st: St::Off,
        server_name: String::new(),
        server_key: String::new(),
        error: String::new(),
        servers: Vec::new(),
        refreshing: false,
        autostart: engine::autostart_enabled(),
        child: None,
        update: None,
        update_busy: false,
        update_failed: false,
        update_note: String::new(),
        tg: None,
        tg_error: String::new(),
    };
    let mut win: Option<Win> = None;
    let mut next_poll = Instant::now() + Duration::from_secs(2);
    let mut next_update = Instant::now() + Duration::from_secs(8);

    event_loop.run(move |event, target, control_flow| {
        let now = Instant::now();

        // раз в 2 секунды проверяем, не упало ли ядро
        if now >= next_poll {
            next_poll = now + Duration::from_secs(2);
            if let Some(c) = app.child.as_mut() {
                if let Ok(Some(code)) = c.try_wait() {
                    app.child = None;
                    engine::stop_sidecar();
                    let tail = engine::log_tail(900);
                    app.error = if tail.is_empty() {
                        format!("Ядро sing-box остановилось ({code}).")
                    } else {
                        format!("Ядро sing-box остановилось ({code}):\n{tail}")
                    };
                    app.st = St::Error;
                    notify(APP, "Соединение оборвалось. Подробности в окне Nyxar.");
                    sync(&app, &tray, &status_item, &toggle_item, &win);
                }
            }
        }
        // обновления: через 8 секунд после старта и затем раз в 6 часов
        if now >= next_update {
            next_update = now + UPDATE_EVERY;
            spawn_update_check(&proxy, false);
        }
        *control_flow = ControlFlow::WaitUntil(next_poll.min(next_update));

        match event {
            Event::NewEvents(StartCause::Init) => {
                std::thread::spawn(update::cleanup);
                if !minimized {
                    win = open_window(target, &proxy);
                }
                apply_tg(&mut app);
                refresh_servers(&proxy, &mut app);
                if app.settings.autoconnect && !app.settings.link.trim().is_empty() {
                    begin_connect(&proxy, &mut app);
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::Show) => show_window(&mut win, target, &proxy),

            Event::UserEvent(UserEvent::Tray(e)) => match e {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
                | TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } => show_window(&mut win, target, &proxy),
                _ => {}
            },

            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == *open_item.id() {
                    show_window(&mut win, target, &proxy);
                } else if e.id == *toggle_item.id() {
                    toggle_connection(&proxy, &mut app);
                    if app.st == St::Error && win.is_none() {
                        show_window(&mut win, target, &proxy);
                    }
                    sync(&app, &tray, &status_item, &toggle_item, &win);
                } else if e.id == *quit_item.id() {
                    engine::stop_core(&mut app.child);
                    win = None;
                    tray = None;
                    *control_flow = ControlFlow::Exit;
                }
            }

            Event::UserEvent(UserEvent::Ui(msg)) => {
                let v: Value = serde_json::from_str(&msg).unwrap_or(Value::Null);
                match v["cmd"].as_str().unwrap_or("") {
                    "toggle" => toggle_connection(&proxy, &mut app),
                    "refresh" => refresh_servers(&proxy, &mut app),
                    "server" => {
                        app.settings.server = v["key"].as_str().unwrap_or("auto").to_string();
                        engine::save_settings(&app.settings);
                        if app.child.is_some() {
                            restart(&proxy, &mut app);
                        }
                    }
                    "set" => {
                        let on = v["value"].as_bool().unwrap_or(false);
                        match v["key"].as_str().unwrap_or("") {
                            "bypass_ru" => {
                                app.settings.bypass_ru = on;
                                engine::save_settings(&app.settings);
                                if app.child.is_some() {
                                    restart(&proxy, &mut app);
                                }
                            }
                            "verbose" => {
                                app.settings.verbose = on;
                                engine::save_settings(&app.settings);
                                if app.child.is_some() {
                                    restart(&proxy, &mut app);
                                }
                            }
                            "tg_proxy" => {
                                app.settings.tg_proxy = on;
                                engine::save_settings(&app.settings);
                                apply_tg(&mut app);
                            }
                            "autoconnect" => {
                                app.settings.autoconnect = on;
                                engine::save_settings(&app.settings);
                            }
                            "auto_update" => {
                                app.settings.auto_update = on;
                                engine::save_settings(&app.settings);
                                maybe_auto_update(&proxy, &mut app);
                            }
                            "autostart" => match engine::set_autostart(on) {
                                Ok(()) => app.autostart = on,
                                Err(m) => {
                                    app.error = m.clone();
                                    notify(APP, &m);
                                }
                            },
                            _ => {}
                        }
                    }
                    "save" => {
                        app.settings.link = v["link"].as_str().unwrap_or("").trim().to_string();
                        engine::save_settings(&app.settings);
                        if app.settings.link.is_empty() {
                            engine::stop_core(&mut app.child);
                            app.servers.clear();
                            app.st = St::Error;
                            app.error = "Вставь ссылку vless:// или адрес подписки.".into();
                        } else {
                            refresh_servers(&proxy, &mut app);
                            restart(&proxy, &mut app);
                        }
                    }
                    "log" => push_log(&win),
                    "folder" => engine::open_data_dir(),
                    "tg_add" => {
                        if app.tg.is_some() {
                            let link = tgproxy::link(&app.settings.tg_secret);
                            if !engine::open_url(&link) {
                                app.tg_error = "Не удалось открыть Telegram. Скопируй ссылку кнопкой рядом.".into();
                            }
                        }
                    }
                    "check_update" => {
                        app.update_note = "Проверяю обновления…".into();
                        spawn_update_check(&proxy, true);
                    }
                    "do_update" => {
                        app.update_failed = false;
                        start_update_install(&proxy, &mut app);
                    }
                    _ => {}
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::Servers(result)) => {
                app.refreshing = false;
                match result {
                    Ok(list) => app.servers = list,
                    Err(m) => {
                        if app.st == St::Off || app.st == St::Error {
                            app.error = m;
                        }
                    }
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::Started(result)) => {
                match result {
                    Ok((c, name, key)) => {
                        engine::stop_core(&mut app.child); // на случай гонки двух подключений
                        app.child = Some(c);
                        app.server_name = name.clone();
                        app.server_key = key;
                        app.st = St::On;
                        app.error.clear();
                        notify(APP, &format!("Подключено: {name}"));
                    }
                    Err(m) => {
                        app.st = St::Error;
                        app.error = m.clone();
                        notify(&format!("{APP}: ошибка"), &m);
                        maybe_auto_update(&proxy, &mut app);
                    }
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::UpdateChecked { manual, result }) => {
                match result {
                    Ok(Some(rel)) => {
                        let is_new = app
                            .update
                            .as_ref()
                            .map(|r| r.version != rel.version)
                            .unwrap_or(true);
                        if is_new {
                            notify(APP, &format!("Доступна версия {}", rel.version));
                        }
                        app.update = Some(rel);
                        app.update_note.clear();
                        maybe_auto_update(&proxy, &mut app);
                    }
                    Ok(None) => {
                        app.update = None;
                        app.update_note = if manual {
                            "Установлена последняя версия".into()
                        } else {
                            String::new()
                        };
                    }
                    Err(m) => {
                        app.update_note = if manual { m } else { String::new() };
                    }
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::UpdateInstalled(result)) => match result {
                Ok(ver) => {
                    notify(APP, &format!("Обновлено до {ver}, перезапускаюсь…"));
                    engine::stop_core(&mut app.child);
                    update::relaunch();
                    win = None;
                    tray = None;
                    *control_flow = ControlFlow::Exit;
                }
                Err(m) => {
                    app.update_busy = false;
                    app.update_failed = true;
                    app.update_note = m.clone();
                    notify(&format!("{APP}: обновление не удалось"), &m);
                    sync(&app, &tray, &status_item, &toggle_item, &win);
                }
            },

            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } => {
                // закрытие окна освобождает WebView (и память), программа остаётся в трее
                if win.as_ref().map(|w| w.window.id() == window_id).unwrap_or(false) {
                    win = None;
                }
            }

            _ => {}
        }
    });
}
