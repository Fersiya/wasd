//! Вся логика без интерфейса: настройки, разбор ссылок, пинг, конфиг и запуск sing-box.

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
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use url::Url;

pub const APP: &str = "Nyxar";

/// Домены, которые при включённой опции идут напрямую, мимо VPN.
pub const RU_DIRECT: &[&str] = &[
    "ru", "su", "xn--p1ai", "yandex.net", "yandex.com", "yastatic.net", "vk.com", "vk.me",
    "vk-cdn.net", "vkuser.net", "userapi.com", "mycdn.me", "avito.st", "wbstatic.net",
    "2gis.com", "kinopoisk.dev",
];

// ───────────────────────── настройки ─────────────────────────

fn d_auto() -> String {
    "auto".into()
}
fn d_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Settings {
    #[serde(default)]
    pub link: String,
    /// "auto" (лучший пинг) или ключ сервера host:port
    #[serde(default = "d_auto")]
    pub server: String,
    #[serde(default = "d_true")]
    pub bypass_ru: bool,
    #[serde(default)]
    pub autoconnect: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            link: String::new(),
            server: d_auto(),
            bypass_ru: true,
            autoconnect: false,
        }
    }
}

pub fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Куда писать settings.json, config.json и лог.
/// Portable: если рядом с exe лежит sing-box.exe (или файл `portable`), всё хранится рядом.
/// Иначе — %LOCALAPPDATA%\Nyxar (обычная установка и единый exe).
pub fn data_dir() -> PathBuf {
    let base = base_dir();
    let portable = base.join("sing-box.exe").exists() || base.join("portable").exists();
    if !portable {
        if let Some(d) = std::env::var_os("LOCALAPPDATA") {
            let p = PathBuf::from(d).join(APP);
            if fs::create_dir_all(&p).is_ok() {
                return p;
            }
        }
    }
    base
}

#[allow(dead_code)]
fn unpack(gz: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz).read_to_end(&mut out)?;
    Ok(out)
}

/// Файлы, вшитые в exe (сжатые gzip). Пустой файл после распаковки пропускается.
#[cfg(feature = "embed")]
const CORE_FILES: &[(&str, &[u8])] = &[
    ("sing-box.exe", include_bytes!("../assets/sing-box.exe.gz")),
    ("wintun.dll", include_bytes!("../assets/wintun.dll.gz")),
    ("libcronet.dll", include_bytes!("../assets/libcronet.dll.gz")),
];

/// Распаковывает вшитое ядро в <data>/core (один раз на версию программы).
#[cfg(feature = "embed")]
fn embedded_core() -> Option<Result<PathBuf, String>> {
    let dir = data_dir().join("core");
    let marker = dir.join(".version");
    let core = dir.join("sing-box.exe");
    let ver = env!("CARGO_PKG_VERSION");
    if core.exists() && fs::read_to_string(&marker).map(|v| v == ver).unwrap_or(false) {
        return Some(Ok(core));
    }
    let res = (|| -> Result<PathBuf, String> {
        fs::create_dir_all(&dir).map_err(|e| format!("Не создалась папка ядра: {e}"))?;
        for (name, gz) in CORE_FILES {
            let bytes = unpack(gz).map_err(|e| format!("Не распаковалось {name}: {e}"))?;
            if bytes.is_empty() {
                continue;
            }
            let tmp = dir.join(format!("{name}.new"));
            fs::write(&tmp, &bytes).map_err(|e| format!("Не записался {name}: {e}"))?;
            fs::rename(&tmp, dir.join(name)).map_err(|e| {
                format!("Не удалось обновить {name} (закрой другие копии Nyxar): {e}")
            })?;
        }
        fs::write(&marker, ver).map_err(|e| format!("Не записалась версия ядра: {e}"))?;
        Ok(core.clone())
    })();
    Some(res)
}

#[cfg(not(feature = "embed"))]
fn embedded_core() -> Option<Result<PathBuf, String>> {
    None
}

/// Путь к sing-box: рядом с exe (portable) или вшитый в программу.
pub fn core_path() -> Result<PathBuf, String> {
    let side = base_dir().join(if cfg!(windows) { "sing-box.exe" } else { "sing-box" });
    if side.exists() {
        return Ok(side);
    }
    embedded_core().unwrap_or_else(|| Err("Рядом с программой нет sing-box.exe".into()))
}

pub fn load_settings() -> Settings {
    fs::read_to_string(data_dir().join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) {
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = fs::write(data_dir().join("settings.json"), text);
    }
}

// ───────────────────── серверы и подписки ─────────────────────

#[derive(Clone, Debug)]
pub struct Server {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub link: String,
    pub ping: Option<u32>,
}

impl Server {
    pub fn key(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn label(&self) -> String {
        match self.ping {
            Some(ms) => format!("{} — {} мс", self.name, ms),
            None => format!("{} — нет ответа", self.name),
        }
    }
}

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

/// Принимает vless://, несколько ссылок или URL подписки.
pub fn fetch_links(src: &str) -> Result<Vec<String>, String> {
    let src = src.trim();
    if src.is_empty() {
        return Err("Ссылка не задана: открой «Настройки»".into());
    }
    let text = if src.starts_with("http://") || src.starts_with("https://") {
        ureq::get(src)
            .timeout(Duration::from_secs(15))
            .set("User-Agent", "nyxar")
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

pub fn parse_server(link: &str) -> Option<Server> {
    let u = Url::parse(link.trim()).ok()?;
    if u.scheme() != "vless" {
        return None;
    }
    let host = u.host_str()?.trim_matches(|c| c == '[' || c == ']').to_string();
    let port = u.port().unwrap_or(443);
    let name = percent_decode_str(u.fragment().unwrap_or(""))
        .decode_utf8_lossy()
        .trim()
        .to_string();
    Some(Server {
        name: if name.is_empty() { host.clone() } else { name },
        host,
        port,
        link: link.trim().to_string(),
        ping: None,
    })
}

pub fn load_servers(src: &str) -> Result<Vec<Server>, String> {
    let servers: Vec<Server> = fetch_links(src)?
        .iter()
        .filter_map(|l| parse_server(l))
        .collect();
    if servers.is_empty() {
        Err("Не удалось разобрать ни одной ссылки".into())
    } else {
        Ok(servers)
    }
}

fn tcp_ping(host: &str, port: u16) -> Option<u32> {
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    let t = Instant::now();
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    Some(t.elapsed().as_millis() as u32)
}

/// Пингует все серверы параллельно (время TCP-подключения).
pub fn ping_all(servers: &mut [Server]) {
    std::thread::scope(|sc| {
        for s in servers.iter_mut() {
            sc.spawn(move || {
                s.ping = tcp_ping(&s.host, s.port);
            });
        }
    });
}

/// "auto" или неизвестный ключ: сервер с лучшим пингом, иначе первый.
pub fn pick_server<'a>(servers: &'a [Server], key: &str) -> Option<&'a Server> {
    if key != "auto" {
        if let Some(s) = servers.iter().find(|s| s.key() == key) {
            return Some(s);
        }
    }
    servers
        .iter()
        .min_by_key(|s| s.ping.unwrap_or(u32::MAX))
}

// ─────────────────────── конфиг sing-box ───────────────────────

pub fn vless_outbound(link: &str) -> Result<Value, String> {
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
            let mut t = json!({
                "type": "ws",
                "path": if g("path").is_empty() { "/" } else { g("path") }
            });
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

    Ok(out)
}

pub fn build_config(outbound: Value, bypass_ru: bool) -> Value {
    let mut dns_rules: Vec<Value> = vec![];
    let mut route_rules: Vec<Value> = vec![
        json!({ "action": "sniff" }),
        json!({ "protocol": "dns", "action": "hijack-dns" }),
        json!({ "ip_is_private": true, "action": "route", "outbound": "direct" }),
    ];
    if bypass_ru {
        dns_rules.push(json!({
            "domain_suffix": RU_DIRECT, "action": "route", "server": "local"
        }));
        route_rules.push(json!({
            "domain_suffix": RU_DIRECT, "action": "route", "outbound": "direct"
        }));
    }

    json!({
        "log": { "level": "warn", "timestamp": true },
        "dns": {
            "servers": [
                { "tag": "remote", "type": "https", "server": "1.1.1.1", "detour": "proxy" },
                { "tag": "local", "type": "udp", "server": "77.88.8.8" }
            ],
            "rules": dns_rules,
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
            "rules": route_rules
        }
    })
}

// ───────────────────────── процесс ядра ─────────────────────────

fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

/// Запускает ядро. Возвращает процесс и название выбранного сервера.
pub fn start_core(s: &Settings) -> Result<(Child, String), String> {
    let core = core_path()?;
    let core_dir = core.parent().map(|p| p.to_path_buf()).unwrap_or_else(base_dir);
    let dir = data_dir();

    let mut servers = load_servers(&s.link)?;
    let known = s.server != "auto" && servers.iter().any(|x| x.key() == s.server);
    if !known {
        ping_all(&mut servers);
    }
    let srv = pick_server(&servers, &s.server)
        .ok_or("Нет доступных серверов")?
        .clone();

    let cfg = build_config(vless_outbound(&srv.link)?, s.bypass_ru);
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
        .current_dir(&core_dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    hide_window(&mut cmd);
    let child = cmd
        .spawn()
        .map_err(|e| format!("Не запустился sing-box: {e}"))?;
    Ok((child, srv.name))
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Последние строки лога sing-box (показываются в окне при ошибке).
pub fn log_tail(max_chars: usize) -> String {
    let bytes = fs::read(data_dir().join("sing-box.log")).unwrap_or_default();
    let text = strip_ansi(&String::from_utf8_lossy(&bytes));
    let mut out: Vec<&str> = Vec::new();
    let mut len = 0;
    for line in text.lines().rev().filter(|l| !l.trim().is_empty()) {
        len += line.len() + 1;
        if len > max_chars && !out.is_empty() {
            break;
        }
        out.push(line.trim());
    }
    out.reverse();
    out.join("\n")
}

/// Открывает папку с настройками и логом.
pub fn open_data_dir() {
    let dir = data_dir();
    let mut c = if cfg!(windows) {
        Command::new("explorer")
    } else {
        Command::new("xdg-open")
    };
    c.arg(dir).stdout(Stdio::null()).stderr(Stdio::null());
    let _ = c.spawn();
}

#[cfg(windows)]
mod win {
    use std::{ptr, thread, time::Duration};

    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
        fn FreeConsole() -> i32;
        fn SetConsoleCtrlHandler(handler: *const core::ffi::c_void, add: i32) -> i32;
        fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
    }

    /// Шлёт Ctrl+C скрытой консоли ядра, чтобы оно само убрало адаптер и маршруты.
    pub fn send_ctrl_c(pid: u32) {
        unsafe {
            FreeConsole();
            if AttachConsole(pid) != 0 {
                SetConsoleCtrlHandler(ptr::null(), 1); // сами Ctrl+C игнорируем
                GenerateConsoleCtrlEvent(0, 0); // CTRL_C_EVENT
                thread::sleep(Duration::from_millis(150));
                FreeConsole();
                SetConsoleCtrlHandler(ptr::null(), 0);
            }
        }
    }
}

/// Мягкая остановка: Ctrl+C и ожидание до 4 секунд, потом принудительно.
pub fn stop_core(child: &mut Option<Child>) {
    let Some(mut c) = child.take() else { return };
    #[cfg(windows)]
    {
        win::send_ctrl_c(c.id());
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = c.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = c.kill();
    let _ = c.wait();
}

// ───────────────────────── автозапуск ─────────────────────────
// Запись в «Автозагрузку» не подходит: программа требует права администратора,
// и Windows пропустит её при входе. Планировщик заданий с высоким уровнем прав — подходит.

pub fn autostart_enabled() -> bool {
    if !cfg!(windows) {
        return false;
    }
    let mut c = Command::new("schtasks");
    c.args(["/Query", "/TN", APP])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_window(&mut c);
    c.status().map(|s| s.success()).unwrap_or(false)
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Автозапуск работает только в Windows".into());
    }
    let mut c = Command::new("schtasks");
    if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        c.args(["/Create", "/TN", APP, "/TR"])
            .arg(format!("\"{}\" --minimized", exe.display()))
            .args(["/SC", "ONLOGON", "/RL", "HIGHEST", "/F"]);
    } else {
        c.args(["/Delete", "/TN", APP, "/F"]);
    }
    c.stdout(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut c);
    match c.status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("Планировщик заданий отказал в доступе".into()),
        Err(e) => Err(format!("Не удалось вызвать schtasks: {e}")),
    }
}

// ─────────────────────────── тесты ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "vless://3f2a1b4c-0000-4000-8000-123456789abc@example.org:443?security=reality&sni=www.example.com&fp=chrome&pbk=PUBKEY&sid=ab12&flow=xtls-rprx-vision&type=tcp#%D0%9D%D0%B8%D0%B4%D0%B5%D1%80%D0%BB%D0%B0%D0%BD%D0%B4%D1%8B";

    #[test]
    fn parses_server_name_and_key() {
        let s = parse_server(LINK).unwrap();
        assert_eq!(s.name, "Нидерланды");
        assert_eq!(s.key(), "example.org:443");
    }

    #[test]
    fn reality_outbound() {
        let o = vless_outbound(LINK).unwrap();
        assert_eq!(o["server"], "example.org");
        assert_eq!(o["flow"], "xtls-rprx-vision");
        assert_eq!(o["tls"]["reality"]["public_key"], "PUBKEY");
        assert_eq!(o["tls"]["server_name"], "www.example.com");
    }

    #[test]
    fn subscription_base64_roundtrip() {
        let plain = format!("{LINK}\n{LINK}\n");
        let b64 = STANDARD.encode(plain.as_bytes());
        let links = fetch_links(&b64).unwrap();
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn config_bypass_toggle() {
        let o = vless_outbound(LINK).unwrap();
        let on = build_config(o.clone(), true);
        let off = build_config(o, false);
        assert_eq!(on["route"]["rules"].as_array().unwrap().len(), 4);
        assert_eq!(off["route"]["rules"].as_array().unwrap().len(), 3);
        assert!(on["dns"]["rules"].as_array().unwrap().len() == 1);
        assert!(off["dns"]["rules"].as_array().unwrap().is_empty());
    }

    #[test]
    fn picks_best_ping_or_selected() {
        let mk = |h: &str, p: Option<u32>| Server {
            name: h.into(),
            host: h.into(),
            port: 443,
            link: String::new(),
            ping: p,
        };
        let v = vec![mk("a", Some(90)), mk("b", Some(30)), mk("c", None)];
        assert_eq!(pick_server(&v, "auto").unwrap().host, "b");
        assert_eq!(pick_server(&v, "c:443").unwrap().host, "c");
        assert_eq!(pick_server(&v, "zzz:1").unwrap().host, "b");
    }

    #[test]
    fn gzip_roundtrip() {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(b"nyxar core bytes").unwrap();
        let gz = enc.finish().unwrap();
        assert_eq!(unpack(&gz).unwrap(), b"nyxar core bytes");
    }

    #[cfg(feature = "embed")]
    #[test]
    fn embedded_core_is_extracted() {
        let tmp = std::env::temp_dir().join(format!("nyxar-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        std::env::set_var("LOCALAPPDATA", &tmp);
        let p = core_path().unwrap();
        assert!(p.ends_with("core/sing-box.exe") || p.ends_with("core\\sing-box.exe"));
        assert!(p.exists());
        assert!(core_path().unwrap() == p); // второй вызов берёт готовое
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn old_settings_file_still_loads() {
        let s: Settings = serde_json::from_str(r#"{"link":"vless://x"}"#).unwrap();
        assert_eq!(s.server, "auto");
        assert!(s.bypass_ru);
        assert!(!s.autoconnect);
    }
}
