#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;
mod probe;
mod saver;
mod tgproxy;
mod update;

use engine::{Server, Settings, APP};
use serde_json::{json, Value};
use std::{
    process::Child,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
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

/// Как часто проверять обновления.
/// Как часто обновлять список серверов из подписки.
const SUB_EVERY: Duration = Duration::from_secs(6 * 3600);
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 3600);

enum UserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    Ui(String),
    Show,
    /// Сколько байт отправлено и получено за подключение
    Traffic(Option<(u64, u64)>, Option<String>, Option<Vec<saver::Conn>>),
    /// Есть ли выход в интернет через туннель
    Health(bool),
    /// Список программ для выбора: JSON-массив или текст ошибки
    Apps(Result<String, String>),
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
    /// Сервер, выбранный автопереключением (не сохраняется в настройках)
    override_key: Option<String>,
    connected_at: Option<Instant>,
    traffic: (u64, u64),
    speed: (u64, u64),
    last_traffic: Option<(u64, u64, Instant)>,
    health_fails: u32,
    last_failover: Option<Instant>,
    /// Сервер, который сейчас выбрала группа умного соединения
    active_key: Option<String>,
    stats_busy: Arc<AtomicBool>,
    health_busy: Arc<AtomicBool>,
    tick: u32,
    pending_autoconnect: bool,
    /// Трафик за месяц и по программам
    usage: saver::Usage,
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
            "auto_switch": self.settings.auto_switch,
            "smart": self.settings.smart,
            "smart_on": self.st == St::On && self.server_key == engine::SMART_KEY,
            "smart_n": engine::smart_candidates(&self.servers).len(),
            "bypass_apps": self.settings.bypass_apps,
            "favs": self.settings.favorites,
            "saver": self.settings.saver,
            "limit_mb": self.settings.limit_mb,
            "month_bytes": self.usage.total,
            "top_apps": self.usage.top(8).into_iter()
                .map(|(name, bytes)| {
                    let bypass = self.settings.bypass_apps.iter().any(|a| a.eq_ignore_ascii_case(&name));
                    json!({ "name": name, "bytes": bytes, "bypass": bypass })
                })
                .collect::<Vec<_>>(),
            "up": self.traffic.0,
            "down": self.traffic.1,
            "up_speed": self.speed.0,
            "down_speed": self.speed.1,
            "uptime": self.connected_at.map(|t| t.elapsed().as_secs()).unwrap_or(0),
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
    let mut s = app.settings.clone();
    if let Some(k) = &app.override_key {
        s.server = k.clone();
    }
    std::thread::spawn(move || {
        let _ = p.send_event(UserEvent::Started(engine::start_core(&s)));
    });
}

/// Скачивает список серверов из подписки и меряет пинг.
/// Пока VPN включён, пинг не меряем (через туннель он ложный) и оставляем прежний.
fn refresh_servers(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    let link = app.settings.link.clone();
    if link.trim().is_empty() {
        return;
    }
    app.refreshing = true;
    let tunnel = app.child.is_some() || app.st == St::Connecting;
    let old: Vec<(String, Option<u32>)> = app.servers.iter().map(|s| (s.key(), s.ping)).collect();
    let p = proxy.clone();
    std::thread::spawn(move || {
        let res = engine::load_servers(&link).map(|mut list| {
            if tunnel {
                for s in list.iter_mut() {
                    s.ping = old.iter().find(|(k, _)| *k == s.key()).and_then(|(_, p)| *p);
                }
            } else {
                engine::ping_all(&mut list);
            }
            list
        });
        let _ = p.send_event(UserEvent::Servers(res));
    });
}

/// Связь пропала: отключаемся, меряем серверы напрямую и подключаемся к лучшему живому.
fn failover(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    engine::stop_core(&mut app.child);
    app.st = St::Connecting;
    app.health_fails = 0;
    app.connected_at = None;
    let p = proxy.clone();
    let mut s = app.settings.clone();
    let current = app.active_key.clone().unwrap_or_else(|| app.server_key.clone());
    std::thread::spawn(move || {
        let res = engine::load_servers(&s.link).and_then(|mut list| {
            engine::ping_all(&mut list);
            let best = list
                .iter()
                .filter(|x| x.ping.is_some() && x.key() != current)
                .min_by_key(|x| x.ping)
                .map(|x| x.key())
                .unwrap_or(current);
            s.server = best;
            engine::start_core(&s)
        });
        if let Ok((_, name, _)) = &res {
            notify(APP, &format!("Связь пропала, переключился на {name}"));
        }
        let _ = p.send_event(UserEvent::Started(res));
    });
}

fn restart(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    app.st = St::Connecting;
    app.error.clear();
    let mut old_child = app.child.take();
    let p = proxy.clone();
    let mut s = app.settings.clone();
    if let Some(k) = &app.override_key {
        s.server = k.clone();
    }
    std::thread::spawn(move || {
        engine::stop_core(&mut old_child);
        let _ = p.send_event(UserEvent::Started(engine::start_core(&s)));
    });
}

fn toggle_connection(proxy: &EventLoopProxy<UserEvent>, app: &mut App) {
    if app.child.is_some() || app.st == St::Connecting {
        app.usage.save(&engine::data_dir());
        let mut old_child = app.child.take();
        std::thread::spawn(move || {
            engine::stop_core(&mut old_child);
        });
        app.st = St::Off;
        app.override_key = None;
        app.connected_at = None;
        app.error.clear();
        maybe_auto_update(proxy, app);
    } else {
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

#[cfg(windows)]
mod ipc {
    use std::{ffi::OsStr, io::Write, os::windows::ffi::OsStrExt, ptr};

    const MUTEX_NAME: &str = "Local\\Nyxar_SingleInstance_Mutex";
    const PIPE_NAME: &str = "\\\\.\\pipe\\Nyxar_SingleInstance_Pipe";
    const ERROR_ALREADY_EXISTS: u32 = 183;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const PIPE_ACCESS_DUPLEX: u32 = 0x00000003;
    const PIPE_TYPE_MESSAGE: u32 = 0x00000004;
    const PIPE_READMODE_MESSAGE: u32 = 0x00000002;
    const PIPE_WAIT: u32 = 0x00000000;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *mut std::ffi::c_void, initial_owner: i32, name: *const u16) -> isize;
        fn GetLastError() -> u32;
        fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buf: u32,
            in_buf: u32,
            timeout: u32,
            attrs: *mut std::ffi::c_void,
        ) -> isize;
        fn ConnectNamedPipe(h_pipe: isize, overlapped: *mut std::ffi::c_void) -> i32;
        fn DisconnectNamedPipe(h_pipe: isize) -> i32;
        fn ReadFile(
            h_file: isize,
            buf: *mut u8,
            len: u32,
            read: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn ensure_single_instance(on_show: impl Fn() + Send + 'static) -> Option<isize> {
        let m_name = wide(MUTEX_NAME);
        let mutex = unsafe { CreateMutexW(ptr::null_mut(), 0, m_name.as_ptr()) };
        if mutex == 0 {
            return None;
        }
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            for _ in 0..10 {
                if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(PIPE_NAME) {
                    let _ = f.write_all(b"show");
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            std::process::exit(0);
        }

        std::thread::spawn(move || {
            let p_name = wide(PIPE_NAME);
            loop {
                let h_pipe = unsafe {
                    CreateNamedPipeW(
                        p_name.as_ptr(),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                        1,
                        512,
                        512,
                        0,
                        ptr::null_mut(),
                    )
                };
                if h_pipe == -1 || h_pipe == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    continue;
                }
                let connected = unsafe { ConnectNamedPipe(h_pipe, ptr::null_mut()) };
                if connected != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED {
                    let mut buf = [0u8; 16];
                    let mut read = 0u32;
                    let ok = unsafe { ReadFile(h_pipe, buf.as_mut_ptr(), buf.len() as u32, &mut read, ptr::null_mut()) };
                    if ok != 0 && &buf[..read as usize] == b"show" {
                        on_show();
                    }
                    unsafe { DisconnectNamedPipe(h_pipe); }
                }
                unsafe { CloseHandle(h_pipe); }
            }
        });

        Some(mutex)
    }
}

// ──────────────────────────── main ────────────────────────────

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    #[cfg(windows)]
    let _mutex = {
        let p = proxy.clone();
        ipc::ensure_single_instance(move || {
            let _ = p.send_event(UserEvent::Show);
        })
    };

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
        override_key: None,
        connected_at: None,
        traffic: (0, 0),
        speed: (0, 0),
        last_traffic: None,
        health_fails: 0,
        last_failover: None,
        active_key: None,
        stats_busy: Arc::new(AtomicBool::new(false)),
        health_busy: Arc::new(AtomicBool::new(false)),
        tick: 0,
        pending_autoconnect: false,
        usage: saver::Usage::load(&engine::data_dir()),
    };
    let mut win: Option<Win> = None;
    let mut next_poll = Instant::now() + Duration::from_secs(2);
    let mut next_update = Instant::now() + Duration::from_secs(8);
    let mut next_sub = Instant::now() + SUB_EVERY;

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
            if app.st == St::On && app.child.is_some() {
                app.tick = app.tick.wrapping_add(1);
                if !app.stats_busy.swap(true, Ordering::Relaxed) {
                    let (p, busy) = (proxy.clone(), app.stats_busy.clone());
                    std::thread::spawn(move || {
                        let snap = engine::snapshot();
                        let totals = snap.as_ref().map(|x| (x.up, x.down));
                        let conns = snap.map(|x| x.conns);
                        let _ = p.send_event(UserEvent::Traffic(totals, engine::active_key(), conns));
                        busy.store(false, Ordering::Relaxed);
                    });
                }
                // раз в ~10 секунд проверяем, что через туннель вообще есть интернет
                if app.settings.auto_switch && app.tick % 5 == 0 && !app.health_busy.swap(true, Ordering::Relaxed) {
                    let (p, busy) = (proxy.clone(), app.health_busy.clone());
                    std::thread::spawn(move || {
                        let _ = p.send_event(UserEvent::Health(engine::tunnel_alive()));
                        busy.store(false, Ordering::Relaxed);
                    });
                }
            }
        }
        // подписка: свежий список раз в 6 часов
        if now >= next_sub {
            next_sub = now + SUB_EVERY;
            if app.st != St::Connecting {
                refresh_servers(&proxy, &mut app);
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
                // сначала меряем серверы без туннеля, потом подключаемся
                app.pending_autoconnect = app.settings.autoconnect && !app.settings.link.trim().is_empty();
                refresh_servers(&proxy, &mut app);
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::Traffic(t, active, conns)) => {
                // умное соединение: показываем, какой сервер сейчас работает, и сообщаем о переходах
                if app.st == St::On && app.server_key == engine::SMART_KEY {
                    if let Some(k) = active {
                        if app.active_key.as_deref() != Some(k.as_str()) {
                            let name = app.servers.iter().find(|x| x.key() == k).map(|x| x.name.clone());
                            if let Some(name) = name {
                                if app.active_key.is_some() {
                                    notify(APP, &format!("Связь переключена на {name}"));
                                }
                                app.server_name = format!("Умный выбор · {name}");
                                app.active_key = Some(k);
                                sync(&app, &tray, &status_item, &toggle_item, &win);
                            }
                        }
                    }
                }
                if let (Some(conns), true) = (conns, app.st == St::On) {
                    let before = app.usage.total;
                    app.usage.feed(&saver::now_month(), &conns);
                    if let Some(level) = app.usage.check_limit(app.settings.limit_mb) {
                        notify(
                            APP,
                            if level >= 2 {
                                "Месячный лимит трафика исчерпан"
                            } else {
                                "Израсходовано больше 80% месячного лимита"
                            },
                        );
                    }
                    // на диск пишем не чаще, чем раз в ~30 секунд
                    if app.tick % 15 == 0 && app.usage.total != before {
                        app.usage.save(&engine::data_dir());
                    }
                }
                if let (Some((up, down)), true) = (t, app.st == St::On) {
                    let n = Instant::now();
                    if let Some((u0, d0, t0)) = app.last_traffic {
                        let dt = n.duration_since(t0).as_secs_f64().max(0.1);
                        app.speed = (
                            (up.saturating_sub(u0) as f64 / dt) as u64,
                            (down.saturating_sub(d0) as f64 / dt) as u64,
                        );
                    }
                    app.last_traffic = Some((up, down, n));
                    app.traffic = (up, down);
                    sync(&app, &tray, &status_item, &toggle_item, &win);
                }
            }

            Event::UserEvent(UserEvent::Apps(res)) => {
                if let Some(w) = &win {
                    let js = match res {
                        Ok(json) => format!("window.__apps({json}, \"\");"),
                        Err(m) => format!("window.__apps([], {});", serde_json::to_string(&m).unwrap_or_else(|_| "\"\"".into())),
                    };
                    let _ = w.webview.evaluate_script(&js);
                }
            }

            Event::UserEvent(UserEvent::Health(ok)) => {
                if app.st == St::On {
                    if ok {
                        app.health_fails = 0;
                    } else {
                        app.health_fails += 1;
                        // не чаще раза в 90 секунд: при полном отсутствии сети переключения бессмысленны
                        let calm = app.last_failover.map(|t| t.elapsed() > Duration::from_secs(90)).unwrap_or(true);
                        if app.health_fails >= 3 && calm && app.settings.link.trim() != "" {
                            app.last_failover = Some(Instant::now());
                            failover(&proxy, &mut app);
                            sync(&app, &tray, &status_item, &toggle_item, &win);
                        }
                    }
                }
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
                    app.usage.save(&engine::data_dir());
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
                        app.override_key = None;
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
                            "smart" => {
                                app.settings.smart = on;
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
                            "saver" => {
                                app.settings.saver = on;
                                engine::save_settings(&app.settings);
                                if app.child.is_some() {
                                    restart(&proxy, &mut app);
                                }
                            }
                            "auto_switch" => {
                                app.settings.auto_switch = on;
                                engine::save_settings(&app.settings);
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
                    "fav" => {
                        if let Some(k) = v["key"].as_str() {
                            let f = &mut app.settings.favorites;
                            if let Some(i) = f.iter().position(|x| x == k) {
                                f.remove(i);
                            } else {
                                f.push(k.to_string());
                            }
                            engine::save_settings(&app.settings);
                        }
                    }
                    "limit" => {
                        app.settings.limit_mb = v["mb"].as_u64().unwrap_or(0).min(10_000_000);
                        // новый лимит — новые предупреждения
                        app.usage.warned = 0;
                        engine::save_settings(&app.settings);
                    }
                    "usage_reset" => {
                        app.usage.reset();
                        app.usage.save(&engine::data_dir());
                    }
                    "log" => push_log(&win),
                    "apps" => {
                        let mut list: Vec<String> = v["list"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                            .unwrap_or_default();
                        list.dedup();
                        app.settings.bypass_apps = list;
                        engine::save_settings(&app.settings);
                        if app.child.is_some() {
                            restart(&proxy, &mut app);
                        }
                    }
                    "list_apps" => {
                        let p = proxy.clone();
                        std::thread::spawn(move || {
                            let _ = p.send_event(UserEvent::Apps(engine::list_apps()));
                        });
                    }
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
                    "ready" => {}
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
                if app.pending_autoconnect {
                    app.pending_autoconnect = false;
                    if app.child.is_none() && app.st != St::Connecting {
                        begin_connect(&proxy, &mut app);
                    }
                }
                sync(&app, &tray, &status_item, &toggle_item, &win);
            }

            Event::UserEvent(UserEvent::Started(result)) => {
                if app.st == St::Off {
                    if let Ok((c, _, _)) = result {
                        std::thread::spawn(move || {
                            engine::stop_core(&mut Some(c));
                        });
                    }
                    sync(&app, &tray, &status_item, &toggle_item, &win);
                    return;
                }
                match result {
                    Ok((c, name, key)) => {
                        engine::stop_core(&mut app.child); // на случай гонки двух подключений
                        app.child = Some(c);
                        app.server_name = name.clone();
                        if app.settings.server != "auto" && app.settings.server != key {
                            app.override_key = Some(key.clone()); // сервер выбран автопереключением
                        }
                        app.server_key = key;
                        app.active_key = None;
                        app.st = St::On;
                        app.error.clear();
                        app.connected_at = Some(Instant::now());
                        app.traffic = (0, 0);
                        app.speed = (0, 0);
                        app.last_traffic = None;
                        app.usage.new_session();
                        app.health_fails = 0;
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
                    app.usage.save(&engine::data_dir());
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
