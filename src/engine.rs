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
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// Подробный лог sing-box (для диагностики, лог быстро растёт)
    #[serde(default)]
    pub verbose: bool,
    /// Ставить обновления с GitHub автоматически (когда VPN не подключён)
    #[serde(default = "d_true")]
    pub auto_update: bool,
    /// Сам переключаться на другой сервер, если связь пропала
    #[serde(default = "d_true")]
    pub auto_switch: bool,
    /// Программы (имена процессов, например game.exe), которые идут мимо VPN
    #[serde(default)]
    pub bypass_apps: Vec<String>,
    /// Локальный прокси для Telegram (обход блокировки через WebSocket)
    #[serde(default)]
    pub tg_proxy: bool,
    /// Секрет прокси (hex, 16 байт), создаётся один раз
    #[serde(default)]
    pub tg_secret: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            link: String::new(),
            server: d_auto(),
            bypass_ru: true,
            autoconnect: false,
            verbose: false,
            auto_update: true,
            auto_switch: true,
            bypass_apps: Vec::new(),
            tg_proxy: false,
            tg_secret: String::new(),
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
    ("xray.exe", include_bytes!("../assets/xray.exe.gz")),
];

/// Завершает sing-box.exe / xray.exe, запущенные из папки ядра Nyxar (остались от прошлого запуска).
fn kill_stale_core(dir: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let d = dir.display().to_string().replace('\'', "''");
        let script = format!(
            "Get-Process sing-box,xray -ErrorAction SilentlyContinue | \
             Where-Object {{ $_.Path -and $_.Path.StartsWith('{d}', [StringComparison]::OrdinalIgnoreCase) }} | \
             Stop-Process -Force"
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .creation_flags(0x0800_0000)
            .status();
    }
    #[cfg(not(windows))]
    let _ = dir;
}

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
            let mut res = fs::rename(&tmp, dir.join(name));
            if res.is_err() {
                // файл занят забытым ядром от прошлого запуска: гасим его и пробуем снова
                kill_stale_core(&dir);
                for _ in 0..8 {
                    std::thread::sleep(Duration::from_millis(400));
                    res = fs::rename(&tmp, dir.join(name));
                    if res.is_ok() {
                        break;
                    }
                }
            }
            res.map_err(|e| {
                let _ = fs::remove_file(&tmp);
                format!("Не удалось обновить {name}: {e}. Заверши процессы sing-box.exe и xray.exe в диспетчере задач.")
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
    /// Уникальный ключ сервера. Раньше был host:port, но в подписках много записей
    /// с одним адресом и портом (разные UUID/параметры), из-за чего выбирались сразу несколько.
    /// Хэш ссылки без имени (#...): имя может меняться при каждом обновлении подписки.
    pub fn key(&self) -> String {
        let base = self.link.split('#').next().unwrap_or("");
        let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a, стабилен между версиями
        for b in base.trim().bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("s{h:016x}")
    }

    fn legacy_key(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// Запись-пустышка: информация о трафике/сроке, закрытая заглушка, адрес-заглушка.
/// Такие серверы отвечают на пинг мгновенно, выигрывают в «Авто», но трафика не дают.
fn is_dummy(s: &Server) -> bool {
    let name = s.name.to_lowercase();
    let host = s.host.to_lowercase();
    let bad_host = host == "localhost"
        || host == "0.0.0.0"
        || host == "::"
        || host == "::1"
        || host.starts_with("127.")
        || host.starts_with("0.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.");
    let bad_uuid = s.link.contains("00000000-0000-0000-0000-000000000000");
    let info_name = [
        "осталось", "истека", "истёк", "истек", "expire", "traffic left", "трафик закон",
        "подписка закончилась",
    ]
    .iter()
    .any(|k| name.contains(k));
    s.port <= 1 || bad_host || bad_uuid || info_name
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

/// User-Agent популярных клиентов. Многие панели подписок отдают заглушку
/// («приложение не поддерживается») тем, кого не знают, поэтому пробуем несколько.
const USER_AGENTS: &[&str] = &[
    "v2rayN/7.4.0",
    "v2rayNG/1.9.40",
    "Happ/2.0.0",
    "Hiddify/2.5.7",
    "nyxar",
];

/// Какой User-Agent сработал в прошлый раз (с него начинаем).
static GOOD_UA: AtomicUsize = AtomicUsize::new(0);

/// Ссылка-заглушка вместо настоящего сервера.
pub fn is_stub(link: &str) -> bool {
    let name = parse_server(link).map(|s| s.name.to_lowercase()).unwrap_or_default();
    [
        "не поддерж",
        "not support",
        "unsupported",
        "update your app",
        "обновите приложение",
    ]
    .iter()
    .any(|k| name.contains(k))
}

fn extract_links(text: &str) -> Vec<String> {
    decode_maybe_b64(text)
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("vless://"))
        .map(String::from)
        .collect()
}

fn download_subscription(url: &str, ua: &str) -> Result<String, ureq::Error> {
    let body = ureq::get(url)
        .timeout(Duration::from_secs(15))
        .set("User-Agent", ua)
        .call()?
        .into_string()
        .map_err(|e| ureq::Error::from(e))?;
    Ok(body)
}

/// Принимает vless://, несколько ссылок или URL подписки.
pub fn fetch_links(src: &str) -> Result<Vec<String>, String> {
    let src = src.trim();
    if src.is_empty() {
        return Err("Ссылка не задана: вставь её в поле внизу окна".into());
    }
    if !(src.starts_with("http://") || src.starts_with("https://")) {
        let links = extract_links(src);
        return if links.is_empty() {
            Err("Не нашёл ни одной ссылки vless://".into())
        } else {
            Ok(links)
        };
    }

    let start = GOOD_UA.load(Ordering::Relaxed) % USER_AGENTS.len();
    let mut stub: Option<String> = None;
    let mut last_err = String::from("Не нашёл ни одной ссылки vless://");
    for i in 0..USER_AGENTS.len() {
        let idx = (start + i) % USER_AGENTS.len();
        match download_subscription(src, USER_AGENTS[idx]) {
            Ok(text) => {
                let (stubs, real): (Vec<String>, Vec<String>) =
                    extract_links(&text).into_iter().partition(|l| is_stub(l));
                if !real.is_empty() {
                    GOOD_UA.store(idx, Ordering::Relaxed);
                    return Ok(real);
                }
                if let Some(s) = stubs.first().and_then(|l| parse_server(l)) {
                    stub = Some(s.name);
                }
            }
            Err(e @ ureq::Error::Transport(_)) => {
                return Err(format!("Не удалось скачать подписку: {e}"));
            }
            Err(e) => last_err = format!("Подписка ответила ошибкой: {e}"),
        }
    }
    match stub {
        Some(name) => Err(format!(
            "Подписка вернула заглушку «{name}» вместо серверов, хотя я пробовал разные приложения. \
             Открой ссылку подписки в браузере и проверь, что там список серверов, \
             или напиши в поддержку провайдера."
        )),
        None => Err(last_err),
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
    let mut seen = std::collections::HashSet::new();
    let mut reasons: Vec<(String, usize)> = Vec::new();
    let mut dummy = 0usize;
    let mut servers: Vec<Server> = Vec::new();
    for s in fetch_links(src)?.iter().filter_map(|l| parse_server(l)) {
        if is_dummy(&s) {
            dummy += 1;
        } else if let Err(why) = link_supported(&s.link) {
            let why = why.to_string();
            match reasons.iter_mut().find(|(r, _)| *r == why) {
                Some(e) => e.1 += 1,
                None => reasons.push((why, 1)),
            }
        } else if seen.insert(s.key()) {
            servers.push(s);
        }
    }
    if servers.is_empty() {
        let detail: String = reasons.iter().map(|(r, n)| format!("\n• {r}: {n}")).collect();
        Err(format!("Нет рабочих серверов. Служебных записей: {dummy}.{}", if detail.is_empty() { String::new() } else { format!("\nОтброшены:{detail}") }))
    } else {
        Ok(servers)
    }
}

/// Пингует все серверы параллельно (TLS-рукопожатие, где возможно, иначе TCP).
/// Выполнять только при выключенном VPN: через туннель замер получается ложным.
pub fn ping_all(servers: &mut [Server]) {
    std::thread::scope(|sc| {
        for s in servers.iter_mut() {
            sc.spawn(move || {
                s.ping = crate::probe::ping(&s.host, s.port, &s.link);
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
        // старые настройки хранили host:port
        if let Some(s) = servers.iter().find(|s| s.legacy_key() == key) {
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

    let enc = g("encryption");
    if !enc.is_empty() && enc != "none" {
        return Err("Шифрование VLESS encryption не поддерживается ядром".into());
    }

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
            if matches!(g("allowInsecure"), "1" | "true") || matches!(g("insecure"), "1" | "true") {
                tls["insecure"] = json!(true);
            }
            out["tls"] = tls;
        }
        "" | "none" => {}
        other => return Err(format!("Защита {other} не поддерживается")),
    }

    let path = if g("path").is_empty() { "/" } else { g("path") };
    match g("type") {
        "" | "tcp" => {
            // tcp с заголовком http-обфускации
            if g("headerType") == "http" {
                let mut t = json!({ "type": "http", "method": "GET", "path": path });
                if !g("host").is_empty() {
                    t["host"] = json!(g("host").split(',').collect::<Vec<_>>());
                }
                out["transport"] = t;
            }
        }
        "ws" => {
            let mut t = json!({ "type": "ws", "path": path });
            if !g("host").is_empty() {
                t["headers"] = json!({ "Host": g("host") });
            }
            out["transport"] = t;
        }
        "grpc" => {
            out["transport"] = json!({ "type": "grpc", "service_name": g("serviceName") });
        }
        "httpupgrade" => {
            let mut t = json!({ "type": "httpupgrade", "path": path });
            if !g("host").is_empty() {
                t["host"] = json!(g("host"));
            }
            out["transport"] = t;
        }
        "h2" | "http" => {
            let mut t = json!({ "type": "http", "path": path });
            if !g("host").is_empty() {
                t["host"] = json!(g("host").split(',').collect::<Vec<_>>());
            }
            out["transport"] = t;
        }
        // раньше такие ссылки молча превращались в обычный tcp: подключение есть, трафика нет
        other => return Err(format!("Транспорт {other} не поддерживается ядром")),
    }

    Ok(out)
}

// ───────────────────────── Xray (xhttp и др.) ─────────────────────────
// sing-box не умеет xhttp и VLESS encryption. Такие серверы обслуживает Xray-core:
// sing-box (TUN) -> socks 127.0.0.1:порт -> xray -> сервер.

struct VlessLink {
    host: String,
    port: u16,
    uuid: String,
    q: HashMap<String, String>,
}

fn parse_vless(link: &str) -> Result<VlessLink, String> {
    let u = Url::parse(link.trim()).map_err(|e| format!("Неверная ссылка: {e}"))?;
    if u.scheme() != "vless" {
        return Err("Нужна ссылка vless://".into());
    }
    let host = u
        .host_str()
        .ok_or("В ссылке нет адреса сервера")?
        .trim_matches(|c| c == '[' || c == ']')
        .to_string();
    let uuid = percent_decode_str(u.username()).decode_utf8_lossy().to_string();
    if uuid.is_empty() {
        return Err("В ссылке нет UUID".into());
    }
    Ok(VlessLink {
        host,
        port: u.port().unwrap_or(443),
        uuid,
        q: u.query_pairs().into_owned().collect(),
    })
}

/// Исходящее соединение для Xray. `addr` подменяет адрес сервера (уже найденный IP), чтобы
/// xray не обращался к DNS через собственный туннель; SNI и Host остаются доменом.
pub fn xray_outbound(link: &str, addr: Option<&str>) -> Result<Value, String> {
    let v = parse_vless(link)?;
    let g = |k: &str| v.q.get(k).map(|s| s.as_str()).unwrap_or("");

    let enc = if g("encryption").is_empty() { "none" } else { g("encryption") };
    let mut user = json!({ "id": v.uuid, "encryption": enc });
    if !g("flow").is_empty() {
        user["flow"] = json!(g("flow"));
    }

    let sni = if !g("sni").is_empty() {
        g("sni").to_string()
    } else if !g("host").is_empty() {
        g("host").to_string()
    } else {
        v.host.clone()
    };
    let fp = if g("fp").is_empty() { "chrome" } else { g("fp") };
    let path = if g("path").is_empty() { "/" } else { g("path") };
    let host_hdr = if g("host").is_empty() { v.host.clone() } else { g("host").to_string() };

    let net = match g("type") {
        "" | "tcp" => "tcp",
        "ws" => "ws",
        "grpc" => "grpc",
        "httpupgrade" => "httpupgrade",
        "xhttp" | "splithttp" => "xhttp",
        other => return Err(format!("Транспорт {other} не поддерживается")),
    };
    if net == "tcp" && g("headerType") == "http" {
        return Err("Транспорт tcp с http-обфускацией не поддерживается".into());
    }

    let mut ss = json!({ "network": net });
    match g("security") {
        "reality" => {
            ss["security"] = json!("reality");
            ss["realitySettings"] = json!({
                "serverName": sni,
                "fingerprint": fp,
                "publicKey": g("pbk"),
                "shortId": g("sid"),
                "spiderX": g("spx"),
            });
        }
        "tls" => {
            ss["security"] = json!("tls");
            let mut tls = json!({ "serverName": sni, "fingerprint": fp });
            if !g("alpn").is_empty() {
                tls["alpn"] = json!(g("alpn").split(',').collect::<Vec<_>>());
            }
            if matches!(g("allowInsecure"), "1" | "true") || matches!(g("insecure"), "1" | "true") {
                tls["allowInsecure"] = json!(true);
            }
            ss["tlsSettings"] = tls;
        }
        "" | "none" => ss["security"] = json!("none"),
        other => return Err(format!("Защита {other} не поддерживается")),
    }

    match net {
        "ws" => ss["wsSettings"] = json!({ "path": path, "host": host_hdr }),
        "grpc" => {
            ss["grpcSettings"] =
                json!({ "serviceName": g("serviceName"), "multiMode": g("mode") == "multi" });
        }
        "httpupgrade" => ss["httpupgradeSettings"] = json!({ "path": path, "host": host_hdr }),
        "xhttp" => {
            let mode = if g("mode").is_empty() { "auto" } else { g("mode") };
            let mut x = json!({ "path": path, "host": host_hdr, "mode": mode });
            if let Ok(extra @ Value::Object(_)) = serde_json::from_str::<Value>(g("extra")) {
                x["extra"] = extra;
            }
            ss["xhttpSettings"] = x;
        }
        _ => {}
    }

    Ok(json!({
        "tag": "proxy",
        "protocol": "vless",
        "settings": { "vnext": [{
            "address": addr.unwrap_or(&v.host),
            "port": v.port,
            "users": [user],
        }]},
        "streamSettings": ss,
    }))
}

/// Подходит ли ссылка хотя бы одному из ядер (sing-box напрямую или через Xray).
pub fn link_supported(link: &str) -> Result<(), String> {
    if vless_outbound(link).is_ok() {
        return Ok(());
    }
    xray_outbound(link, None).map(|_| ())
}

/// Нужен ли для ссылки Xray (sing-box её не потянет).
pub fn needs_xray(link: &str) -> bool {
    vless_outbound(link).is_err() && xray_outbound(link, None).is_ok()
}

pub fn xray_config(outbound: Value, port: u16, verbose: bool) -> Value {
    json!({
        "log": { "loglevel": if verbose { "info" } else { "warning" } },
        "inbounds": [{
            "tag": "socks-in",
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": true, "ip": "127.0.0.1" }
        }],
        "outbounds": [outbound, { "tag": "direct", "protocol": "freedom" }]
    })
}

/// Что добавить к конфигу sing-box, когда сервер работает через Xray.
pub struct Sidecar {
    pub xray_cfg: Value,
    pub outbound: Value,
    /// Правила маршрута: трафик самого xray к серверу идёт мимо туннеля (иначе петля).
    pub rules: Vec<Value>,
}

pub fn sidecar_plan(link: &str, ip: std::net::IpAddr, port: u16, verbose: bool) -> Result<Sidecar, String> {
    let xray_cfg = xray_config(xray_outbound(link, Some(&ip.to_string()))?, port, verbose);
    let cidr = match ip {
        std::net::IpAddr::V4(_) => format!("{ip}/32"),
        std::net::IpAddr::V6(_) => format!("{ip}/128"),
    };
    Ok(Sidecar {
        xray_cfg,
        outbound: json!({
            "type": "socks", "tag": "proxy",
            "server": "127.0.0.1", "server_port": port, "version": "5"
        }),
        rules: vec![
            json!({ "ip_cidr": [cidr], "action": "route", "outbound": "direct" }),
            json!({ "process_name": ["xray.exe", "xray"], "action": "route", "outbound": "direct" }),
        ],
    })
}

fn resolve_server(host: &str, port: u16) -> Result<std::net::IpAddr, String> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(ip);
    }
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Не удалось определить адрес сервера {host}: {e}"))?
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .map(|a| a.ip())
        .ok_or_else(|| format!("Не удалось определить адрес сервера {host}"))
}

fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("Нет свободного порта: {e}"))?;
    l.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

static XRAY: std::sync::Mutex<Option<Child>> = std::sync::Mutex::new(None);

/// Останавливает Xray, если он запущен.
pub fn stop_sidecar() {
    let taken = XRAY.lock().ok().and_then(|mut g| g.take());
    if let Some(mut c) = taken {
        let _ = c.kill();
        let _ = c.wait();
    }
}

fn log_tail_of(file: &str, max_chars: usize) -> String {
    let bytes = fs::read(data_dir().join(file)).unwrap_or_default();
    let text = strip_ansi(&String::from_utf8_lossy(&bytes));
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out: Vec<&str> = Vec::new();
    let mut len = 0;
    for l in lines.iter().rev() {
        len += l.len() + 1;
        if len > max_chars && !out.is_empty() {
            break;
        }
        out.push(l.trim());
    }
    out.reverse();
    out.join("\n")
}

fn start_xray(core_dir: &std::path::Path, cfg: &Value, port: u16) -> Result<(), String> {
    let exe = core_dir.join(if cfg!(windows) { "xray.exe" } else { "xray" });
    if !exe.exists() {
        return Err("Рядом с программой нет xray.exe (нужен для серверов xhttp)".into());
    }
    let dir = data_dir();
    let cfg_path = dir.join("xray.json");
    fs::write(&cfg_path, serde_json::to_string_pretty(cfg).unwrap_or_default())
        .map_err(|e| format!("Не записался xray.json: {e}"))?;
    let log = fs::File::create(dir.join("xray.log")).map_err(|e| format!("Не создался xray.log: {e}"))?;
    let log_err = log.try_clone().map_err(|e| format!("Не создался xray.log: {e}"))?;

    let mut cmd = Command::new(&exe);
    cmd.arg("run")
        .arg("-c")
        .arg(&cfg_path)
        .current_dir(core_dir)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);
    hide_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("Не запустился xray: {e}"))?;

    // ждём, пока xray начнёт слушать порт
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(Some(code)) = child.try_wait() {
            return Err(format!("xray остановился ({code}):\n{}", log_tail_of("xray.log", 700)));
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("xray не запустился за 8 секунд:\n{}", log_tail_of("xray.log", 700)));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Ok(mut g) = XRAY.lock() {
        *g = Some(child);
    }
    Ok(())
}

static API_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Добавляет в конфиг программы мимо VPN и локальный API статистики.
pub fn apply_extras(cfg: &mut Value, apps: &[String], api_port: u16) {
    let names: Vec<String> = apps
        .iter()
        .map(|a| a.trim().trim_matches('"').to_string())
        .filter(|a| !a.is_empty())
        .collect();
    if !names.is_empty() {
        if let Some(rules) = cfg["route"]["rules"].as_array_mut() {
            rules.insert(1, json!({ "process_name": names, "action": "route", "outbound": "direct" }));
        }
    }
    if api_port != 0 {
        cfg["experimental"] = json!({ "clash_api": { "external_controller": format!("127.0.0.1:{api_port}") } });
    }
}

/// Сколько байт отправлено и получено за текущее подключение.
pub fn traffic() -> Option<(u64, u64)> {
    let port = API_PORT.load(Ordering::Relaxed);
    if port == 0 {
        return None;
    }
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_millis(1500)).build();
    let body = agent.get(&format!("http://127.0.0.1:{port}/connections")).call().ok()?.into_string().ok()?;
    let v: Value = serde_json::from_str(&body).ok()?;
    Some((v["uploadTotal"].as_u64()?, v["downloadTotal"].as_u64()?))
}

/// Проверка, что через туннель есть выход в интернет.
pub fn tunnel_alive() -> bool {
    for addr in ["1.1.1.1:443", "8.8.8.8:443"] {
        if let Ok(a) = addr.parse() {
            if TcpStream::connect_timeout(&a, Duration::from_secs(4)).is_ok() {
                return true;
            }
        }
    }
    false
}

pub fn build_config(outbound: Value, bypass_ru: bool, verbose: bool) -> Value {
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
        "log": { "level": if verbose { "info" } else { "warn" }, "timestamp": true },
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
pub fn start_core(s: &Settings) -> Result<(Child, String, String), String> {
    let core = core_path()?;
    let core_dir = core.parent().map(|p| p.to_path_buf()).unwrap_or_else(base_dir);
    let dir = data_dir();

    // забытое ядро от прошлого запуска держит адаптер и порты: гасим один раз за запуск
    static CLEANED: std::sync::Once = std::sync::Once::new();
    CLEANED.call_once(|| kill_stale_core(&core_dir));

    let mut servers = load_servers(&s.link)?;
    let known = s.server != "auto" && servers.iter().any(|x| x.key() == s.server);
    if !known {
        ping_all(&mut servers);
    }
    let srv = pick_server(&servers, &s.server)
        .ok_or("Нет доступных серверов")?
        .clone();

    stop_sidecar();
    let cfg = if needs_xray(&srv.link) {
        let v = parse_vless(&srv.link)?;
        let ip = resolve_server(&v.host, v.port)?;
        let port = free_port()?;
        let plan = sidecar_plan(&srv.link, ip, port, s.verbose)?;
        start_xray(&core_dir, &plan.xray_cfg, port)?;
        let mut cfg = build_config(plan.outbound, s.bypass_ru, s.verbose);
        if let Some(rules) = cfg["route"]["rules"].as_array_mut() {
            for (i, r) in plan.rules.into_iter().enumerate() {
                rules.insert(1 + i, r); // сразу после sniff
            }
        }
        cfg
    } else {
        build_config(vless_outbound(&srv.link)?, s.bypass_ru, s.verbose)
    };
    let mut cfg = cfg;
    let api = free_port().unwrap_or(0);
    API_PORT.store(api, Ordering::Relaxed);
    apply_extras(&mut cfg, &s.bypass_apps, api);
    let cfg_path = dir.join("config.json");
    fs::write(&cfg_path, serde_json::to_string_pretty(&cfg).unwrap_or_default())
        .map_err(|e| format!("Не записался config.json: {e}"))?;

    let log = fs::File::create(dir.join("sing-box.log"))
        .map_err(|e| { stop_sidecar(); format!("Не создался sing-box.log: {e}") })?;
    let log_err = log
        .try_clone()
        .map_err(|e| { stop_sidecar(); format!("Не создался sing-box.log: {e}") })?;

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
        .map_err(|e| { stop_sidecar(); format!("Не запустился sing-box: {e}") })?;
    let key = srv.key();
    Ok((child, srv.name, key))
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
    #[cfg(windows)]
    {
        // ShellExecute открывает папку в проводнике поверх окон и работает и от администратора
        if shell::open(&dir) {
            return;
        }
        let _ = Command::new("explorer")
            .arg(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("xdg-open")
            .arg(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// Открывает адрес (например tg://…) программой по умолчанию.
pub fn open_url(url: &str) -> bool {
    #[cfg(windows)]
    {
        shell::open(std::path::Path::new(url))
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        false
    }
}

#[cfg(windows)]
mod shell {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::Path, ptr};

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// true, если оболочка приняла запрос (результат больше 32).
    pub fn open(path: &Path) -> bool {
        let op = wide(OsStr::new("open"));
        let file = wide(path.as_os_str());
        // 1 = SW_SHOWNORMAL
        let r = unsafe { ShellExecuteW(0, op.as_ptr(), file.as_ptr(), ptr::null(), ptr::null(), 1) };
        r > 32
    }
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
    // Если процесса sing-box нет, xray не трогаем: он мог быть только что запущен новым подключением.
    let Some(mut c) = child.take() else { return };
    #[cfg(windows)]
    {
        win::send_ctrl_c(c.id());
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = c.try_wait() {
                stop_sidecar();
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let _ = c.kill();
    let _ = c.wait();
    stop_sidecar();
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
        assert_eq!(s.key(), parse_server(LINK).unwrap().key());
        // имя не влияет на ключ
        let renamed = LINK.split('#').next().unwrap().to_string() + "#other";
        assert_eq!(parse_server(&renamed).unwrap().key(), s.key());
    }

    #[test]
    fn same_host_port_gives_different_keys() {
        let a = LINK.to_string();
        let b = LINK.replace("3f2a1b4c", "aaaaaaaa");
        let (sa, sb) = (parse_server(&a).unwrap(), parse_server(&b).unwrap());
        assert_eq!((sa.host.clone(), sa.port), (sb.host.clone(), sb.port));
        assert_ne!(sa.key(), sb.key());
    }

    #[test]
    fn dummy_servers_are_dropped() {
        let loopback = "vless://u@127.0.0.1:443?security=tls#x";
        let info = "vless://u@example.org:443?security=tls#%D0%9E%D1%81%D1%82%D0%B0%D0%BB%D0%BE%D1%81%D1%8C%205%20%D0%B4%D0%BD%D0%B5%D0%B9";
        assert!(is_dummy(&parse_server(loopback).unwrap()));
        assert!(is_dummy(&parse_server(info).unwrap()));
        assert!(!is_dummy(&parse_server(LINK).unwrap()));
    }

    #[test]
    fn xhttp_goes_through_xray() {
        let l = LINK.replace("type=tcp", "type=xhttp&mode=packet-up&path=%2Fx&extra=%7B%22xPaddingBytes%22%3A%22100-1000%22%7D");
        assert!(vless_outbound(&l).is_err());
        assert!(needs_xray(&l));
        assert!(link_supported(&l).is_ok());
        let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();
        let plan = sidecar_plan(&l, ip, 12345, false).unwrap();
        let ob = &plan.xray_cfg["outbounds"][0];
        assert_eq!(ob["streamSettings"]["network"], "xhttp");
        assert_eq!(ob["streamSettings"]["xhttpSettings"]["mode"], "packet-up");
        assert_eq!(ob["streamSettings"]["xhttpSettings"]["path"], "/x");
        assert_eq!(ob["streamSettings"]["xhttpSettings"]["extra"]["xPaddingBytes"], "100-1000");
        // адрес подменён на IP, SNI и Host остаются доменом
        assert_eq!(ob["settings"]["vnext"][0]["address"], "203.0.113.7");
        assert_eq!(ob["streamSettings"]["xhttpSettings"]["host"], "example.org");
        assert_eq!(plan.xray_cfg["inbounds"][0]["port"], 12345);
        assert_eq!(plan.outbound["type"], "socks");
        assert_eq!(plan.outbound["server_port"], 12345);
        assert_eq!(plan.rules[0]["ip_cidr"][0], "203.0.113.7/32");
        // обычные ссылки Xray не нужны
        assert!(!needs_xray(LINK));
    }

    #[test]
    fn vless_encryption_goes_through_xray() {
        let l = LINK.replace("&type=tcp", "&type=tcp&encryption=mlkem768x25519plus.native.0rtt.abc");
        assert!(needs_xray(&l));
        let ob = xray_outbound(&l, None).unwrap();
        assert_eq!(
            ob["settings"]["vnext"][0]["users"][0]["encryption"],
            "mlkem768x25519plus.native.0rtt.abc"
        );
    }

    #[test]
    fn unknown_transport_unsupported_by_both() {
        let l = LINK.replace("type=tcp", "type=kcp");
        assert!(link_supported(&l).is_err());
    }

    #[test]
    fn unsupported_transport_is_error() {
        let l = LINK.replace("type=tcp", "type=xhttp");
        assert!(vless_outbound(&l).is_err());
        let l = LINK.replace("type=tcp", "type=httpupgrade&path=%2Fx");
        assert_eq!(vless_outbound(&l).unwrap()["transport"]["type"], "httpupgrade");
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
        let on = build_config(o.clone(), true, false);
        let off = build_config(o, false, false);
        assert_eq!(on["route"]["rules"].as_array().unwrap().len(), 4);
        assert_eq!(off["route"]["rules"].as_array().unwrap().len(), 3);
        assert!(on["dns"]["rules"].as_array().unwrap().len() == 1);
        assert!(off["dns"]["rules"].as_array().unwrap().is_empty());
    }

    #[test]
    fn extras_add_app_rule_and_api() {
        let mut cfg = build_config(json!({"type":"direct","tag":"proxy"}), true, false);
        apply_extras(&mut cfg, &["game.exe".into(), "  ".into(), "\"Steam.exe\"".into()], 9090);
        let rules = cfg["route"]["rules"].as_array().unwrap();
        assert_eq!(rules[0]["action"], "sniff");
        assert_eq!(rules[1]["process_name"], json!(["game.exe", "Steam.exe"]));
        assert_eq!(rules[1]["outbound"], "direct");
        assert_eq!(cfg["experimental"]["clash_api"]["external_controller"], "127.0.0.1:9090");
    }

    #[test]
    fn extras_empty_changes_nothing() {
        let base = build_config(json!({"type":"direct","tag":"proxy"}), true, false);
        let mut cfg = base.clone();
        apply_extras(&mut cfg, &[], 0);
        assert_eq!(cfg, base);
    }

    #[test]
    fn picks_best_ping_or_selected() {
        let mk = |h: &str, p: Option<u32>| Server {
            name: h.into(),
            host: h.into(),
            port: 443,
            link: format!("vless://u@{h}:443"),
            ping: p,
        };
        let v = vec![mk("a", Some(90)), mk("b", Some(30)), mk("c", None)];
        assert_eq!(pick_server(&v, "auto").unwrap().host, "b");
        assert_eq!(pick_server(&v, &v[2].key()).unwrap().host, "c");
        assert_eq!(pick_server(&v, "c:443").unwrap().host, "c"); // старый формат
        assert_eq!(pick_server(&v, "zzz:1").unwrap().host, "b");
    }

    #[test]
    fn detects_stub_servers() {
        let stub = "vless://0000@stub.example:443?security=tls#%D0%9F%D1%80%D0%B8%D0%BB%D0%BE%D0%B6%D0%B5%D0%BD%D0%B8%D0%B5%20%D0%BD%D0%B5%20%D0%BF%D0%BE%D0%B4%D0%B4%D0%B5%D1%80%D0%B6%D0%B8%D0%B2%D0%B0%D0%B5%D1%82%D1%81%D1%8F";
        assert_eq!(parse_server(stub).unwrap().name, "Приложение не поддерживается");
        assert!(is_stub(stub));
        assert!(!is_stub(LINK));
    }

    #[test]
    fn extracts_only_vless_links() {
        let text = format!("vmess://abc\n{LINK}\n\ntrojan://x\n");
        assert_eq!(extract_links(&text), vec![LINK.to_string()]);
    }

    #[test]
    fn verbose_changes_log_level() {
        let o = vless_outbound(LINK).unwrap();
        assert_eq!(build_config(o.clone(), true, false)["log"]["level"], "warn");
        assert_eq!(build_config(o, true, true)["log"]["level"], "info");
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
