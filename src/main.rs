#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;

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

enum UserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    Ui(String),
    Show,
    Started(Result<(Child, String), String>),
    Servers(Result<Vec<Server>, String>),
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
    error: String,
    servers: Vec<Server>,
    autostart: bool,
    child: Option<Child>,
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
                .map(|s| json!({ "key": s.key(), "label": s.label() }))
                .collect::<Vec<_>>(),
            "selected": self.settings.server,
            "bypass_ru": self.settings.bypass_ru,
            "autostart": self.autostart,
            "autoconnect": self.settings.autoconnect,
            "link": self.settings.link,
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
        .with_inner_size(LogicalSize::new(420.0, 720.0))
        .with_resizable(false)
        .with_maximizable(false)
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

fn begin_connect(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    app.st = St::Connecting;
    app.error.clear();
    let p = proxy.clone();
    let s = app.settings.clone();
    std::thread::spawn(move || {
        let _ = p.send_event(UserEvent::Started(engine::start_core(&s)));
    });
}

fn begin_refresh(proxy: &EventLoopProxy<UserEvent>, link: String) {
    if link.trim().is_empty() {
        return;
    }
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
    } else if app.st != St::Connecting {
        if app.settings.link.trim().is_empty() {
            app.st = St::Error;
            app.error = "Сначала добавь ссылку или подписку в поле внизу окна.".into();
        } else {
            begin_connect(proxy, app);
        }
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
        error: String::new(),
        servers: Vec::new(),
        autostart: engine::autostart_enabled(),
        child: None,
    };
    let mut win: Option<Win> = None;
    let mut next_poll = Instant::now() + Duration::from_secs(2);

    event_loop.run(move |event, target, control_flow| {
        // раз в 2 секунды проверяем, не упало ли ядро
        let now = Instant::now();
        if now >= next_poll {
            next_poll = now + Duration::from_secs(2);
            if let Some(c) = app.child.as_mut() {
                if let Ok(Some(code)) = c.try_wait() {
                    app.child = None;
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
        *control_flow = ControlFlow::WaitUntil(next_poll);

        match event {
            Event::NewEvents(StartCause::Init) => {
                if !minimized {
                    win = open_window(target, &proxy);
                }
                begin_refresh(&proxy, app.settings.link.clone());
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
                    "refresh" => begin_refresh(&proxy, app.settings.link.clone()),
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
                            "autoconnect" => {
                                app.settings.autoconnect = on;
                                engine::save_settings(&app.settings);
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
                            begin_refresh(&proxy, app.settings.link.clone());
                            restart(&proxy, &mut app);
                        }
                    }
                    "log" => engine::open_data_dir(),
                    _ => {}
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::Servers(result)) => {
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
                    Ok((c, name)) => {
                        engine::stop_core(&mut app.child); // на случай гонки двух подключений
                        app.child = Some(c);
                        app.server_name = name.clone();
                        app.st = St::On;
                        app.error.clear();
                        notify(APP, &format!("Подключено: {name}"));
                    }
                    Err(m) => {
                        app.st = St::Error;
                        app.error = m.clone();
                        notify(&format!("{APP}: ошибка"), &m);
                    }
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

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
