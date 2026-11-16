#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};
use url::Url;
use wry::{WebView, WebViewBuilder};

// ───────────────────────── настройки ─────────────────────────

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    #[serde(default)]
    link: String,
}

fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_settings() -> Settings {
    fs::read_to_string(base_dir().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(s: &Settings) {
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = fs::write(base_dir().join("settings.json"), text);
    }
}

// ───────────────────── vless:// и подписки ─────────────────────

fn decode_maybe_b64(text: &str) -> String {
    if text.contains("vless://") {
        return text.to_string();
    }
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        if let Ok(bytes) = engine.decode(compact.as_bytes()) {
            if let Ok(s) = String::from_utf8(bytes) {
                return s;
            }
        }
    }
    text.to_string()
}

/// Принимает vless:// ссылку, несколько ссылок или URL подписки.
fn fetch_links(src: &str) -> Result<Vec<String>, String> {
    let src = src.trim();
    if src.is_empty() {
        return Err("Ссылка не задана: открой «Настройки»".into());
    }
    let text = if src.starts_with("http://") || src.starts_with("https://") {
        ureq::get(src)
            .timeout(Duration::from_secs(15))
            .set("User-Agent", "rust-light-vpn")
            .call()
            .map_err(|e| format!("Не удалось скачать подписку: {e}"))?
            .into_string()
            .map_err(|e| format!("Подписка не читается: {e}"))?
    } else {
        src.to_string()
    };
    let links: Vec<String> = decode_maybe_b64(&text)
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("vless://"))
        .map(String::from)
        .collect();
    if links.is_empty() {
        Err("Не нашёл ни одной ссылки vless://".into())
    } else {
        Ok(links)
    }
}

/// Возвращает outbound для sing-box и имя сервера (из #фрагмента).
fn vless_outbound(link: &str) -> Result<(Value, String), String> {
    let u = Url::parse(link.trim()).map_err(|e| format!("Неверная ссылка: {e}"))?;
    if u.scheme() != "vless" {
        return Err("Нужна ссылка vless://".into());
    }
    let host = u
        .host_str()
        .ok_or("В ссылке нет адреса сервера")?
        .trim_matches(|c| c == '[' || c == ']')
        .to_string();
    let port = u.port().unwrap_or(443);
    let uuid = percent_decode_str(u.username()).decode_utf8_lossy().to_string();
    if uuid.is_empty() {
        return Err("В ссылке нет UUID".into());
    }
    let name = percent_decode_str(u.fragment().unwrap_or(""))
        .decode_utf8_lossy()
        .to_string();

    let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
    let g = |k: &str| q.get(k).map(|s| s.as_str()).unwrap_or("");

    let mut out = json!({
        "type": "vless",
        "tag": "proxy",
        "server": host,
        "server_port": port,
        "uuid": uuid,
        "packet_encoding": "xudp",
    });
    if !g("flow").is_empty() {
        out["flow"] = json!(g("flow"));
    }

    let sni = if !g("sni").is_empty() {
        g("sni").to_string()
    } else if !g("host").is_empty() {
        g("host").to_string()
    } else {
        host.clone()
    };
    let fp = if g("fp").is_empty() { "chrome" } else { g("fp") };

    match g("security") {
        "reality" => {
            out["tls"] = json!({
                "enabled": true,
                "server_name": sni,
                "utls": { "enabled": true, "fingerprint": fp },
                "reality": {
                    "enabled": true,
                    "public_key": g("pbk"),
                    "short_id": g("sid"),
                }
            });
        }
        "tls" => {
            let mut tls = json!({
                "enabled": true,
                "server_name": sni,
                "utls": { "enabled": true, "fingerprint": fp },
            });
            if !g("alpn").is_empty() {
                tls["alpn"] = json!(g("alpn").split(',').collect::<Vec<_>>());
            }
            out["tls"] = tls;
        }
        _ => {}
    }

    match g("type") {
        "ws" => {
            let mut t = json!({ "type": "ws", "path": if g("path").is_empty() { "/" } else { g("path") } });
            if !g("host").is_empty() {
                t["headers"] = json!({ "Host": g("host") });
            }
            out["transport"] = t;
        }
        "grpc" => {
            out["transport"] = json!({ "type": "grpc", "service_name": g("serviceName") });
        }
        _ => {}
    }

    Ok((out, name))
}

fn build_config(outbound: Value) -> Value {
    json!({
        "log": { "level": "warn", "timestamp": true },
        "dns": {
            "servers": [
                { "tag": "remote", "type": "https", "server": "1.1.1.1", "detour": "proxy" },
                { "tag": "local", "type": "udp", "server": "77.88.8.8" }
            ],
            "final": "remote"
        },
        "inbounds": [{
            "type": "tun",
            "tag": "tun-in",
            "address": ["172.19.0.1/30"],
            "auto_route": true,
            "strict_route": true,
            "stack": "mixed"
        }],
        "outbounds": [
            outbound,
            { "type": "direct", "tag": "direct" }
        ],
        "route": {
            "auto_detect_interface": true,
            "default_domain_resolver": "local",
            "final": "proxy",
            "rules": [
                { "action": "sniff" },
                { "protocol": "dns", "action": "hijack-dns" },
                { "ip_is_private": true, "action": "route", "outbound": "direct" }
            ]
        }
    })
}

// ───────────────────────── sing-box ─────────────────────────

fn start_core(src: &str) -> Result<(Child, String), String> {
    let dir = base_dir();
    let core = dir.join(if cfg!(windows) { "sing-box.exe" } else { "sing-box" });
    if !core.exists() {
        return Err("Рядом с программой нет sing-box.exe".into());
    }

    let links = fetch_links(src)?;
    let (outbound, name) = vless_outbound(&links[0])?;
    let cfg = build_config(outbound);

    let cfg_path = dir.join("config.json");
    fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap_or_default())
        .map_err(|e| format!("Не записался config.json: {e}"))?;

    let log = fs::File::create(dir.join("sing-box.log"))
        .map_err(|e| format!("Не создался sing-box.log: {e}"))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("Не создался sing-box.log: {e}"))?;

    let mut cmd = Command::new(core);
    cmd.arg("run")
        .arg("-c")
        .arg(&cfg_path)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("Не запустился sing-box: {e}"))?;
    Ok((child, name))
}

fn stop_core(child: &mut Option<Child>) {
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

// ─────────────────────────── UI ───────────────────────────

enum UserEvent {
    Menu(MenuEvent),
    Save(String),
    Started(Result<(Child, String), String>),
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
        let _ = t.set_tooltip(Some(format!("Rust Light VPN — {text}")));
    }
}

fn begin_connect(proxy: &EventLoopProxy<UserEvent>, src: String) {
    let p = proxy.clone();
    std::thread::spawn(move || {
        let _ = p.send_event(UserEvent::Started(start_core(&src)));
    });
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
<p>Подойдёт ключ <code>vless://…</code> или URL подписки. Берётся первый сервер.</p>
<textarea id="l" spellcheck="false"></textarea><br>
<button onclick="save()">Сохранить и подключить</button>
<script>
  document.getElementById('l').value = __LINK__;
  function save() { window.ipc.postMessage(document.getElementById('l').value.trim()); }
</script>"##
        .replace("__LINK__", &link_js)
}

fn main() {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(UserEvent::Menu(e));
        }));
    }

    let status = MenuItem::new("Отключено", false, None);
    let toggle = MenuItem::new("Подключить", true, None);
    let settings_item = MenuItem::new("Настройки…", true, None);
    let quit = MenuItem::new("Выход", true, None);

    let menu = Menu::new();
    let _ = menu.append(&status);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&toggle);
    let _ = menu.append(&settings_item);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);

    let mut tray = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Rust Light VPN — отключено")
            .with_icon(make_icon(false))
            .build()
            .expect("не удалось создать иконку в трее"),
    );

    let mut settings = load_settings();
    let mut child: Option<Child> = None;
    let mut connecting = false;
    let mut win: Option<(Window, WebView)> = None;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(2));

        match event {
            // раз в 2 секунды проверяем, не упало ли ядро
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                if let Some(c) = child.as_mut() {
                    if let Ok(Some(code)) = c.try_wait() {
                        child = None;
                        apply_state(
                            &tray,
                            &status,
                            &toggle,
                            false,
                            &format!("ядро остановилось ({code}), см. sing-box.log"),
                        );
                    }
                }
            }

            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == *toggle.id() {
                    if child.is_some() {
                        stop_core(&mut child);
                        apply_state(&tray, &status, &toggle, false, "отключено");
                    } else if !connecting {
                        connecting = true;
                        status.set_text("подключение…");
                        begin_connect(&proxy, settings.link.clone());
                    }
                } else if e.id == *settings_item.id() {
                    if let Some((w, _)) = win.as_ref() {
                        w.set_focus();
                    } else {
                        let window = WindowBuilder::new()
                            .with_title("Rust Light VPN — настройки")
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
                } else if e.id == *quit.id() {
                    stop_core(&mut child);
                    win = None;
                    tray = None;
                    *control_flow = ControlFlow::Exit;
                }
            }

            Event::UserEvent(UserEvent::Save(link)) => {
                settings.link = link;
                save_settings(&settings);
                win = None;
                stop_core(&mut child);
                if !connecting {
                    connecting = true;
                    status.set_text("подключение…");
                    begin_connect(&proxy, settings.link.clone());
                }
            }

            Event::UserEvent(UserEvent::Started(result)) => {
                connecting = false;
                match result {
                    Ok((c, name)) => {
                        child = Some(c);
                        let label = if name.is_empty() {
                            "подключено".to_string()
                        } else {
                            format!("подключено: {name}")
                        };
                        apply_state(&tray, &status, &toggle, true, &label);
                    }
                    Err(msg) => apply_state(&tray, &status, &toggle, false, &msg),
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
