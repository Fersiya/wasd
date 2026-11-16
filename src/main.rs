#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod engine;

use engine::{Server, Settings, APP};
use std::{
    process::Child,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
    Icon, TrayIcon, TrayIconBuilder,
};
use wry::{WebView, WebViewBuilder};

enum UserEvent {
    Menu(MenuEvent),
    Save(String),
    Started(Result<(Child, String), String>),
    Servers(Result<Vec<Server>, String>),
}

// ─────────────────────────── helpers ───────────────────────────

fn notify(title: &str, body: &str) {
    let (t, b) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new().summary(&t).body(&b).show();
    });
}

fn make_icon(on: bool) -> Icon {
    let (r, g, b) = if on { (46u8, 204u8, 113u8) } else { (149u8, 165u8, 166u8) };
    let n = 32u32;
    let mut rgba = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            if dx * dx + dy * dy <= 14.0 * 14.0 {
                rgba.extend_from_slice(&[r, g, b, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, n, n).expect("иконка")
}

fn apply_state(
    tray: &Option<TrayIcon>,
    status: &MenuItem,
    toggle: &MenuItem,
    on: bool,
    text: &str,
) {
    status.set_text(text);
    toggle.set_text(if on { "Отключить" } else { "Подключить" });
    if let Some(t) = tray {
        let _ = t.set_icon(Some(make_icon(on)));
        let _ = t.set_tooltip(Some(format!("{APP} — {text}")));
    }
}

fn begin_connect(proxy: &EventLoopProxy<UserEvent>, settings: &Settings) {
    let p = proxy.clone();
    let s = settings.clone();
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

fn restart(
    proxy: &EventLoopProxy<UserEvent>,
    settings: &Settings,
    child: &mut Option<Child>,
    connecting: &mut bool,
    status: &MenuItem,
) {
    engine::stop_core(child);
    if !*connecting {
        *connecting = true;
        status.set_text("подключение…");
        begin_connect(proxy, settings);
    }
}

/// Перестраивает подменю «Сервер»: Авто, список с пингом, обновление.
fn fill_servers(
    sub: &Submenu,
    auto: &CheckMenuItem,
    refresh: &MenuItem,
    servers: &[Server],
    selected: &str,
) -> Vec<(CheckMenuItem, String)> {
    while sub.remove_at(0).is_some() {}
    auto.set_checked(selected == "auto");
    let _ = sub.append(auto);
    let _ = sub.append(&PredefinedMenuItem::separator());
    let mut items = Vec::new();
    for s in servers {
        let item = CheckMenuItem::new(s.label(), true, s.key() == selected, None);
        let _ = sub.append(&item);
        items.push((item, s.key()));
    }
    if !servers.is_empty() {
        let _ = sub.append(&PredefinedMenuItem::separator());
    }
    let _ = sub.append(refresh);
    items
}

fn mark_selected(auto: &CheckMenuItem, items: &[(CheckMenuItem, String)], selected: &str) {
    auto.set_checked(selected == "auto");
    for (item, key) in items {
        item.set_checked(key == selected);
    }
}

fn settings_html(link: &str) -> String {
    let link_js = serde_json::to_string(link)
        .unwrap_or_else(|_| "\"\"".into())
        .replace("</", "<\\/");
    r##"<!doctype html>
<meta charset="utf-8">
<style>
  body { font: 14px "Segoe UI", sans-serif; margin: 18px; background: #1e1e1e; color: #eee; }
  p { color: #aaa; margin: 6px 0 10px; }
  textarea { width: 100%; height: 150px; box-sizing: border-box; background: #2b2b2b; color: #eee;
             border: 1px solid #444; border-radius: 6px; padding: 8px; font: 12px Consolas, monospace; }
  button { margin-top: 12px; padding: 8px 18px; border: 0; border-radius: 6px;
           background: #2ecc71; color: #111; font-weight: 600; cursor: pointer; }
</style>
<b>Ссылка на подписку или vless://</b>
<p>Подойдёт ключ <code>vless://…</code> или URL подписки. Сервер выбирается в меню трея.</p>
<textarea id="l" spellcheck="false"></textarea><br>
<button onclick="save()">Сохранить и подключить</button>
<script>
  document.getElementById('l').value = __LINK__;
  function save() { window.ipc.postMessage(document.getElementById('l').value.trim()); }
</script>"##
        .replace("__LINK__", &link_js)
}

// ──────────────────────────── main ────────────────────────────

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Menu(e));
        }));
    }

    let mut settings = engine::load_settings();

    let status = MenuItem::new("отключено", false, None);
    let toggle = MenuItem::new("Подключить", true, None);
    let server_menu = Submenu::new("Сервер", true);
    let auto_item = CheckMenuItem::new("Авто (лучший пинг)", true, settings.server == "auto", None);
    let refresh_item = MenuItem::new("Обновить список и пинг", true, None);
    let settings_item = MenuItem::new("Ссылка и подписка…", true, None);
    let ru_item = CheckMenuItem::new("Россия напрямую", true, settings.bypass_ru, None);
    let autostart_item =
        CheckMenuItem::new("Запускать с Windows", true, engine::autostart_enabled(), None);
    let autoconnect_item =
        CheckMenuItem::new("Подключаться при запуске", true, settings.autoconnect, None);
    let quit = MenuItem::new("Выход", true, None);

    let mut server_items = fill_servers(&server_menu, &auto_item, &refresh_item, &[], &settings.server);

    let menu = Menu::new();
    let _ = menu.append(&status);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&toggle);
    let _ = menu.append(&server_menu);
    let _ = menu.append(&settings_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&ru_item);
    let _ = menu.append(&autostart_item);
    let _ = menu.append(&autoconnect_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);

    let mut tray = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(format!("{APP} — отключено"))
            .with_icon(make_icon(false))
            .build()
            .expect("не удалось создать иконку в трее"),
    );

    let mut child: Option<Child> = None;
    let mut connecting = false;
    let mut win: Option<(Window, WebView)> = None;

    begin_refresh(&proxy, settings.link.clone());
    if settings.autoconnect && !settings.link.trim().is_empty() {
        connecting = true;
        status.set_text("подключение…");
        begin_connect(&proxy, &settings);
    }

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(2));

        match event {
            // раз в 2 секунды проверяем, не упало ли ядро
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(c) = child.as_mut() {
                    if let Ok(Some(code)) = c.try_wait() {
                        child = None;
                        apply_state(&tray, &status, &toggle, false, "соединение оборвалось");
                        notify(APP, &format!("Соединение оборвалось ({code}). Подробности в sing-box.log"));
                    }
                }
            }

            Event::UserEvent(UserEvent::Menu(e)) => {
                let id = e.id.clone();
                let mut new_server: Option<String> = None;
                let mut changed_routing = false;

                if id == *toggle.id() {
                    if child.is_some() {
                        engine::stop_core(&mut child);
                        apply_state(&tray, &status, &toggle, false, "отключено");
                    } else if !connecting {
                        connecting = true;
                        status.set_text("подключение…");
                        begin_connect(&proxy, &settings);
                    }
                } else if id == *settings_item.id() {
                    if let Some((w, _)) = win.as_ref() {
                        w.set_focus();
                    } else {
                        let window = WindowBuilder::new()
                            .with_title(format!("{APP} — ссылка и подписка"))
                            .with_inner_size(LogicalSize::new(520.0, 330.0))
                            .with_resizable(false)
                            .build(target)
                            .expect("окно");
                        let p = proxy.clone();
                        let webview = WebViewBuilder::new()
                            .with_html(settings_html(&settings.link))
                            .with_ipc_handler(move |req| {
                                let _ = p.send_event(UserEvent::Save(req.body().to_string()));
                            })
                            .build(&window)
                            .expect("webview");
                        win = Some((window, webview));
                    }
                } else if id == *refresh_item.id() {
                    begin_refresh(&proxy, settings.link.clone());
                } else if id == *auto_item.id() {
                    new_server = Some("auto".into());
                } else if let Some((_, key)) = server_items.iter().find(|(i, _)| id == *i.id()) {
                    new_server = Some(key.clone());
                } else if id == *ru_item.id() {
                    settings.bypass_ru = ru_item.is_checked();
                    engine::save_settings(&settings);
                    changed_routing = true;
                } else if id == *autoconnect_item.id() {
                    settings.autoconnect = autoconnect_item.is_checked();
                    engine::save_settings(&settings);
                } else if id == *autostart_item.id() {
                    let want = autostart_item.is_checked();
                    if let Err(msg) = engine::set_autostart(want) {
                        autostart_item.set_checked(!want);
                        notify(APP, &msg);
                    }
                } else if id == *quit.id() {
                    engine::stop_core(&mut child);
                    win = None;
                    tray = None;
                    *control_flow = ControlFlow::Exit;
                    return;
                }

                if let Some(key) = new_server {
                    settings.server = key;
                    engine::save_settings(&settings);
                    mark_selected(&auto_item, &server_items, &settings.server);
                    changed_routing = true;
                }
                if changed_routing && child.is_some() {
                    restart(&proxy, &settings, &mut child, &mut connecting, &status);
                }
            }

            Event::UserEvent(UserEvent::Save(link)) => {
                settings.link = link;
                engine::save_settings(&settings);
                win = None;
                begin_refresh(&proxy, settings.link.clone());
                restart(&proxy, &settings, &mut child, &mut connecting, &status);
            }

            Event::UserEvent(UserEvent::Servers(result)) => {
                if let Ok(list) = result {
                    server_items = fill_servers(
                        &server_menu,
                        &auto_item,
                        &refresh_item,
                        &list,
                        &settings.server,
                    );
                }
            }

            Event::UserEvent(UserEvent::Started(result)) => {
                connecting = false;
                match result {
                    Ok((c, name)) => {
                        engine::stop_core(&mut child); // на случай гонки двух подключений
                        child = Some(c);
                        apply_state(&tray, &status, &toggle, true, &format!("подключено: {name}"));
                        notify(APP, &format!("Подключено: {name}"));
                    }
                    Err(msg) => {
                        apply_state(&tray, &status, &toggle, false, "ошибка");
                        notify(&format!("{APP} — ошибка"), &msg);
                    }
                }
            }

            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } => {
                if win.as_ref().map(|(w, _)| w.id() == window_id).unwrap_or(false) {
                    win = None;
                }
            }

            _ => {}
        }
    });
}
