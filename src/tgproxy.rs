//! Локальный MTProto-прокси для Telegram поверх WebSocket.
//!
//! Идея та же, что у Flowseal/tg-ws-proxy (MIT): Telegram подключается к 127.0.0.1:1443
//! как к обычному MTProto-прокси (obfuscated2, секрет `dd…`), а мы вместо прямого TCP
//! до дата-центра открываем WebSocket на kws{N}.web.telegram.org, который не блокируется.
//! Данные перешифровываются: клиентский ключ (с секретом) -> ключ канала до Telegram.

use aes::Aes256;
use ctr::cipher::{KeyIvInit, StreamCipher};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

pub const PORT: u16 = 1443;
const HANDSHAKE_LEN: usize = 64;
const SKIP_LEN: usize = 8;
const PREKEY_LEN: usize = 32;
const IV_LEN: usize = 16;
const PROTO_TAG_POS: usize = 56;
const DC_IDX_POS: usize = 60;

const TAG_ABRIDGED: [u8; 4] = [0xef; 4];
const TAG_INTERMEDIATE: [u8; 4] = [0xee; 4];
const TAG_SECURE: [u8; 4] = [0xdd; 4];

/// Адрес, на котором живут все kws*.web.telegram.org
const WS_IP: &str = "149.154.167.220";

type Ctr = ctr::Ctr128BE<Aes256>;

fn dc_ip(dc: u32, test: bool) -> Option<&'static str> {
    Some(match (test, dc) {
        (false, 1) => "149.154.175.50",
        (false, 2) => "149.154.167.51",
        (false, 3) => "149.154.175.100",
        (false, 4) => "149.154.167.91",
        (false, 5) => "149.154.171.5",
        (false, 203) => "91.105.192.100",
        (true, 1) => "149.154.175.10",
        (true, 2) => "149.154.167.40",
        (true, 3) => "149.154.175.117",
        _ => return None,
    })
}

fn ws_domains(dc: u32, media: bool) -> Vec<String> {
    let dc = if dc == 203 { 2 } else { dc };
    if media {
        vec![format!("kws{dc}-1.web.telegram.org"), format!("kws{dc}.web.telegram.org")]
    } else {
        vec![format!("kws{dc}.web.telegram.org")]
    }
}

fn random(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    getrandom::getrandom(&mut v).expect("системный генератор случайных чисел недоступен");
    v
}

pub fn new_secret() -> String {
    hex(&random(16))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Ссылка для добавления прокси в Telegram.
pub fn link(secret: &str) -> String {
    format!("tg://proxy?server=127.0.0.1&port={PORT}&secret=dd{secret}")
}

// ─────────────────────────── крипто ───────────────────────────

fn ctr(key: &[u8], iv: &[u8]) -> Ctr {
    Ctr::new(key.into(), iv.into())
}

pub struct Handshake {
    pub dc: u32,
    pub media: bool,
    pub test: bool,
    pub proto: [u8; 4],
    prekey_iv: Vec<u8>,
}

/// Проверяет 64 байта от клиента: расшифровывает ключом sha256(prekey + secret).
pub fn parse_handshake(hs: &[u8], secret: &[u8]) -> Option<Handshake> {
    if hs.len() != HANDSHAKE_LEN {
        return None;
    }
    let prekey_iv = &hs[SKIP_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN];
    let key = Sha256::new().chain_update(&prekey_iv[..PREKEY_LEN]).chain_update(secret).finalize();
    let mut dec = hs.to_vec();
    ctr(&key, &prekey_iv[PREKEY_LEN..]).apply_keystream(&mut dec);
    let proto: [u8; 4] = dec[PROTO_TAG_POS..PROTO_TAG_POS + 4].try_into().ok()?;
    if ![TAG_ABRIDGED, TAG_INTERMEDIATE, TAG_SECURE].contains(&proto) {
        return None;
    }
    let idx = i16::from_le_bytes([dec[DC_IDX_POS], dec[DC_IDX_POS + 1]]);
    let mut dc = (idx as i32).unsigned_abs();
    let test = dc >= 10000;
    if test {
        dc -= 10000;
    }
    Some(Handshake { dc, media: idx < 0, test, proto, prekey_iv: prekey_iv.to_vec() })
}

/// 64 случайных байта для канала до Telegram, в хвосте зашифрованы тег протокола и номер DC.
pub fn relay_init(proto: [u8; 4], dc_idx: i16) -> Vec<u8> {
    loop {
        let mut rnd = random(HANDSHAKE_LEN);
        if rnd[0] == 0xef
            || [b"HEAD".as_slice(), b"POST", b"GET ", &[0xee; 4], &[0xdd; 4], &[0x16, 3, 1, 2], b"OPTI"]
                .contains(&&rnd[..4])
            || rnd[4..8] == [0, 0, 0, 0]
        {
            continue;
        }
        let mut enc = rnd.clone();
        ctr(&rnd[SKIP_LEN..SKIP_LEN + PREKEY_LEN], &rnd[SKIP_LEN + PREKEY_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN])
            .apply_keystream(&mut enc);
        let mut plain = [0u8; 8];
        plain[..4].copy_from_slice(&proto);
        plain[4..6].copy_from_slice(&dc_idx.to_le_bytes());
        plain[6..].copy_from_slice(&random(2));
        for i in 0..8 {
            // keystream = encrypted ^ original
            rnd[PROTO_TAG_POS + i] = plain[i] ^ (enc[PROTO_TAG_POS + i] ^ rnd[PROTO_TAG_POS + i]);
        }
        return rnd;
    }
}

pub struct Crypto {
    clt_dec: Ctr,
    clt_enc: Ctr,
    tg_enc: Ctr,
    tg_dec: Ctr,
}

impl Crypto {
    pub fn new(hs: &Handshake, secret: &[u8], relay: &[u8]) -> Self {
        let sha = |p: &[u8]| Sha256::new().chain_update(p).chain_update(secret).finalize();
        let mut clt_dec = ctr(&sha(&hs.prekey_iv[..PREKEY_LEN]), &hs.prekey_iv[PREKEY_LEN..]);
        clt_dec.apply_keystream(&mut [0u8; HANDSHAKE_LEN]);
        let rev: Vec<u8> = hs.prekey_iv.iter().rev().copied().collect();
        let clt_enc = ctr(&sha(&rev[..PREKEY_LEN]), &rev[PREKEY_LEN..]);

        let r = &relay[SKIP_LEN..SKIP_LEN + PREKEY_LEN + IV_LEN];
        let mut tg_enc = ctr(&r[..PREKEY_LEN], &r[PREKEY_LEN..]);
        tg_enc.apply_keystream(&mut [0u8; HANDSHAKE_LEN]);
        let rr: Vec<u8> = r.iter().rev().copied().collect();
        let tg_dec = ctr(&rr[..PREKEY_LEN], &rr[PREKEY_LEN..]);
        Self { clt_dec, clt_enc, tg_enc, tg_dec }
    }
}

// ───────────────── нарезка потока на MTProto-пакеты ─────────────────

/// Режет поток на пакеты транспорта MTProto, чтобы каждый ушёл отдельным WS-кадром.
pub struct Splitter {
    proto: [u8; 4],
    cipher: Vec<u8>,
    plain: Vec<u8>,
    disabled: bool,
}

impl Splitter {
    pub fn new(proto: [u8; 4]) -> Self {
        Self { proto, cipher: Vec::new(), plain: Vec::new(), disabled: false }
    }

    /// Длина пакета: Some(n>0) — готов, None — не хватает данных, Some(0) — не умеем резать.
    fn next_len(&self, off: usize, avail: usize) -> Option<usize> {
        if avail == 0 {
            return None;
        }
        if self.proto == TAG_ABRIDGED {
            let first = self.plain[off];
            let (payload, header) = if first == 0x7f || first == 0xff {
                if avail < 4 {
                    return None;
                }
                let n = u32::from_le_bytes([self.plain[off + 1], self.plain[off + 2], self.plain[off + 3], 0]);
                (n as usize * 4, 4)
            } else {
                ((first & 0x7f) as usize * 4, 1)
            };
            if payload == 0 {
                return Some(0);
            }
            return if avail < header + payload { None } else { Some(header + payload) };
        }
        if self.proto == TAG_INTERMEDIATE || self.proto == TAG_SECURE {
            if avail < 4 {
                return None;
            }
            let p = &self.plain[off..off + 4];
            let n = (u32::from_le_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff) as usize;
            if n == 0 {
                return Some(0);
            }
            return if avail < 4 + n { None } else { Some(4 + n) };
        }
        Some(0)
    }

    pub fn split(&mut self, chunk: &[u8], plain: &[u8]) -> Vec<Vec<u8>> {
        if chunk.is_empty() {
            return vec![];
        }
        if self.disabled {
            return vec![chunk.to_vec()];
        }
        self.cipher.extend_from_slice(chunk);
        self.plain.extend_from_slice(plain);
        let mut parts = Vec::new();
        let (mut off, total) = (0, self.cipher.len());
        while off < total {
            match self.next_len(off, total - off) {
                None => break,
                Some(0) => {
                    parts.push(self.cipher[off..].to_vec());
                    off = total;
                    self.disabled = true;
                    break;
                }
                Some(n) => {
                    parts.push(self.cipher[off..off + n].to_vec());
                    off += n;
                }
            }
        }
        if off > 0 {
            self.cipher.drain(..off);
            self.plain.drain(..off);
        }
        parts
    }

    pub fn flush(&mut self) -> Vec<u8> {
        self.plain.clear();
        std::mem::take(&mut self.cipher)
    }
}

// ─────────────────────────── WebSocket ───────────────────────────

pub fn ws_frame(opcode: u8, data: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = Vec::with_capacity(data.len() + 14);
    f.push(0x80 | opcode);
    match data.len() {
        n if n < 126 => f.push(0x80 | n as u8),
        n if n < 65536 => {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(&mask);
    f.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    f
}

/// Разбор кадров сервера (без маски) из накопленного буфера.
/// Возвращает (opcode, payload, fin, сколько байт съели).
pub fn ws_parse(buf: &[u8]) -> Option<(u8, Vec<u8>, bool, usize)> {
    if buf.len() < 2 {
        return None;
    }
    let (fin, op) = (buf[0] & 0x80 != 0, buf[0] & 0x0f);
    let masked = buf[1] & 0x80 != 0;
    let mut len = (buf[1] & 0x7f) as usize;
    let mut pos = 2;
    if len == 126 {
        if buf.len() < 4 {
            return None;
        }
        len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        pos = 4;
    } else if len == 127 {
        if buf.len() < 10 {
            return None;
        }
        len = u64::from_be_bytes(buf[2..10].try_into().unwrap()) as usize;
        pos = 10;
    }
    let mask = if masked {
        if buf.len() < pos + 4 {
            return None;
        }
        pos += 4;
        Some([buf[pos - 4], buf[pos - 3], buf[pos - 2], buf[pos - 1]])
    } else {
        None
    };
    if buf.len() < pos + len {
        return None;
    }
    let mut payload = buf[pos..pos + len].to_vec();
    if let Some(m) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }
    Some((op, payload, fin, pos + len))
}

/// TLS-соединение, которым могут пользоваться два потока: один читает, другой пишет.
struct Tls {
    conn: Mutex<rustls::ClientConnection>,
    sock: TcpStream,
}

impl Tls {
    fn connect(ip: &str, domain: &str, timeout: Duration) -> io::Result<Self> {
        let addr: SocketAddr = format!("{ip}:443").parse().map_err(|_| io::ErrorKind::InvalidInput)?;
        let sock = TcpStream::connect_timeout(&addr, timeout)?;
        sock.set_nodelay(true).ok();
        sock.set_read_timeout(Some(timeout))?;
        sock.set_write_timeout(Some(timeout))?;
        let name = rustls::pki_types::ServerName::try_from(domain.to_string())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        let mut conn = rustls::ClientConnection::new(tls_config(), name)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        let mut s = &sock;
        while conn.is_handshaking() {
            conn.complete_io(&mut s)?;
        }
        Ok(Self { conn: Mutex::new(conn), sock })
    }

    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut c = self.conn.lock().unwrap();
        c.writer().write_all(data)?;
        let mut s = &self.sock;
        while c.wants_write() {
            c.write_tls(&mut s)?;
        }
        Ok(())
    }

    fn read(&self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            {
                let mut c = self.conn.lock().unwrap();
                match c.reader().read(out) {
                    Ok(n) => return Ok(n),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            let mut tmp = [0u8; 16384];
            let n = (&self.sock).read(&mut tmp)?;
            if n == 0 {
                return Ok(0);
            }
            let mut c = self.conn.lock().unwrap();
            let mut cur = &tmp[..n];
            while !cur.is_empty() {
                c.read_tls(&mut cur)?;
                c.process_new_packets()
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            }
            let mut s = &self.sock;
            while c.wants_write() {
                c.write_tls(&mut s)?;
            }
        }
    }
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    use std::sync::OnceLock;
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("версии TLS")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

struct Ws {
    tls: Tls,
    buf: Mutex<Vec<u8>>,
}

#[derive(Debug)]
enum WsError {
    /// Сервер ответил не 101 (например, 302): WebSocket для этого DC недоступен.
    Status(u16),
    Io(io::Error),
}

impl Ws {
    fn connect(ip: &str, domain: &str, path: &str) -> Result<Self, WsError> {
        let tls = Tls::connect(ip, domain, Duration::from_secs(8)).map_err(WsError::Io)?;
        let key = base64_std(&random(16));
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {domain}\r\nUser-Agent: Mozilla/5.0\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Protocol: binary\r\nOrigin: https://web.telegram.org\r\n\r\n"
        );
        tls.write_all(req.as_bytes()).map_err(WsError::Io)?;
        let mut head = Vec::new();
        let mut one = [0u8; 1];
        // читаем заголовки до \r\n\r\n по байту: лишнего не заберём
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > 16384 {
                return Err(WsError::Io(io::Error::new(io::ErrorKind::InvalidData, "слишком длинный ответ")));
            }
            match tls.read(&mut one) {
                Ok(0) => return Err(WsError::Io(io::ErrorKind::UnexpectedEof.into())),
                Ok(_) => head.push(one[0]),
                Err(e) => return Err(WsError::Io(e)),
            }
        }
        let text = String::from_utf8_lossy(&head);
        let status: u16 = text.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        if status != 101 {
            return Err(WsError::Status(status));
        }
        // дальше таймаут не нужен: соединение живёт, пока его не закроет клиент
        tls.sock.set_read_timeout(None).ok();
        tls.sock.set_write_timeout(Some(Duration::from_secs(30))).ok();
        Ok(Self { tls, buf: Mutex::new(Vec::new()) })
    }

    fn send(&self, data: &[u8]) -> io::Result<()> {
        let m: [u8; 4] = random(4).try_into().unwrap();
        self.tls.write_all(&ws_frame(2, data, m))
    }

    /// Следующая порция данных или None, если сервер закрыл канал.
    fn recv(&self) -> io::Result<Option<Vec<u8>>> {
        let mut frag: Vec<u8> = Vec::new();
        loop {
            let parsed = ws_parse(&self.buf.lock().unwrap());
            match parsed {
                Some((op, payload, fin, used)) => {
                    self.buf.lock().unwrap().drain(..used);
                    match op {
                        8 => return Ok(None),
                        9 => {
                            let m: [u8; 4] = random(4).try_into().unwrap();
                            self.tls.write_all(&ws_frame(10, &payload, m))?;
                        }
                        10 => {}
                        _ => {
                            frag.extend_from_slice(&payload);
                            if fin {
                                return Ok(Some(frag));
                            }
                        }
                    }
                }
                None => {
                    let mut tmp = [0u8; 16384];
                    let n = self.tls.read(&mut tmp)?;
                    if n == 0 {
                        return Ok(None);
                    }
                    self.buf.lock().unwrap().extend_from_slice(&tmp[..n]);
                }
            }
        }
    }

    fn shutdown(&self) {
        let _ = self.tls.sock.shutdown(Shutdown::Both);
    }
}

fn base64_std(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

// ─────────────────────────── сервер ───────────────────────────

pub struct Proxy {
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<Vec<TcpStream>>>,
}

impl Proxy {
    /// Запускает прокси на 127.0.0.1:PORT. Ошибка, если порт занят.
    pub fn start(secret_hex: &str) -> Result<Self, String> {
        let secret = unhex(secret_hex).filter(|s| s.len() == 16).ok_or("Неверный секрет прокси")?;
        let listener = TcpListener::bind(("127.0.0.1", PORT))
            .map_err(|e| format!("Порт {PORT} занят или недоступен: {e}. Закрой tg-ws-proxy или другой прокси."))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let list = clients.clone();
        let secret = Arc::new(secret);
        thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((sock, _)) => {
                        sock.set_nonblocking(false).ok();
                        if let Ok(c) = sock.try_clone() {
                            let mut l = list.lock().unwrap();
                            // забываем уже закрытые подключения
                            l.retain(|s| s.peer_addr().is_ok());
                            l.push(c);
                        }
                        let (secret, flag) = (secret.clone(), flag.clone());
                        thread::spawn(move || {
                            let _ = handle(sock, &secret, &flag);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(100)),
                    Err(_) => thread::sleep(Duration::from_millis(500)),
                }
            }
        });
        Ok(Self { stop, clients })
    }

    /// Останавливает приём и рвёт текущие подключения (Telegram сам переподключится).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        for c in self.clients.lock().unwrap().drain(..) {
            let _ = c.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.stop();
    }
}

enum Remote {
    Ws(Ws),
    Tcp(TcpStream),
}

impl Remote {
    fn shutdown(&self) {
        match self {
            Remote::Ws(w) => w.shutdown(),
            Remote::Tcp(s) => {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

fn open_remote(hs: &Handshake, relay: &[u8]) -> io::Result<Remote> {
    let path = if hs.test { "/apiws_test" } else { "/apiws" };
    if !hs.test || hs.dc <= 3 {
        for domain in ws_domains(hs.dc, hs.media) {
            match Ws::connect(WS_IP, &domain, path) {
                Ok(ws) => {
                    ws.send(relay)?;
                    return Ok(Remote::Ws(ws));
                }
                Err(_) => continue,
            }
        }
    }
    // запасной путь: напрямую в дата-центр (работает, если он не заблокирован)
    let ip = dc_ip(hs.dc, hs.test).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "неизвестный DC"))?;
    let addr: SocketAddr = format!("{ip}:443").parse().unwrap();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(8))?;
    s.set_nodelay(true).ok();
    s.write_all(relay)?;
    Ok(Remote::Tcp(s))
}

fn handle(mut client: TcpStream, secret: &[u8], stop: &AtomicBool) -> io::Result<()> {
    client.set_nodelay(true).ok();
    client.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut init = [0u8; HANDSHAKE_LEN];
    client.read_exact(&mut init)?;
    let Some(hs) = parse_handshake(&init, secret) else {
        // чужой клиент или неверный секрет: молча закрываем
        return Ok(());
    };
    client.set_read_timeout(None)?;

    let dc_idx = if hs.media { -(hs.dc as i16) } else { hs.dc as i16 };
    let relay = relay_init(hs.proto, dc_idx);
    let mut cr = Crypto::new(&hs, secret, &relay);
    let remote = Arc::new(open_remote(&hs, &relay)?);

    // клиент -> Telegram
    let up = {
        let (remote, mut client) = (remote.clone(), client.try_clone()?);
        let mut splitter = Splitter::new(hs.proto);
        let (mut dec, mut enc) = (std::mem::replace(&mut cr.clt_dec, ctr(&[0; 32], &[0; 16])), std::mem::replace(&mut cr.tg_enc, ctr(&[0; 32], &[0; 16])));
        thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            loop {
                let n = match client.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let mut plain = buf[..n].to_vec();
                dec.apply_keystream(&mut plain);
                let mut cipher = plain.clone();
                enc.apply_keystream(&mut cipher);
                let ok = match &*remote {
                    Remote::Ws(ws) => splitter.split(&cipher, &plain).iter().all(|p| ws.send(p).is_ok()),
                    Remote::Tcp(s) => (&*s).write_all(&cipher).is_ok(),
                };
                if !ok {
                    break;
                }
            }
            if let Remote::Ws(ws) = &*remote {
                let tail = splitter.flush();
                if !tail.is_empty() {
                    let _ = ws.send(&tail);
                }
            }
            // закрываем обе стороны, чтобы разбудить поток чтения
            let _ = client.shutdown(Shutdown::Both);
            remote.shutdown();
        })
    };

    // Telegram -> клиент
    let mut down = |data: &mut Vec<u8>| -> io::Result<()> {
        cr.tg_dec.apply_keystream(data);
        cr.clt_enc.apply_keystream(data);
        client.write_all(data)
    };
    let result = match &*remote {
        Remote::Ws(ws) => loop {
            if stop.load(Ordering::Relaxed) {
                break Ok(());
            }
            match ws.recv() {
                Ok(Some(mut d)) => {
                    if let Err(e) = down(&mut d) {
                        break Err(e);
                    }
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        },
        Remote::Tcp(s) => {
            let mut buf = vec![0u8; 65536];
            let mut s = s;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break Ok(());
                }
                match s.read(&mut buf) {
                    Ok(0) => break Ok(()),
                    Ok(n) => {
                        let mut d = buf[..n].to_vec();
                        if let Err(e) = down(&mut d) {
                            break Err(e);
                        }
                    }
                    Err(e) => break Err(e),
                }
            }
        }
    };
    remote.shutdown();
    let _ = client.shutdown(Shutdown::Both);
    let _ = up.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает клиентское рукопожатие так, как это делает Telegram.
    fn client_hello(secret: &[u8], proto: [u8; 4], dc_idx: i16) -> (Vec<u8>, Ctr, Ctr) {
        let mut hs = random(64);
        hs[0] = 0x11;
        hs[4..8].copy_from_slice(&[1, 2, 3, 4]);
        let pk_iv = hs[8..56].to_vec();
        let key = |p: &[u8]| Sha256::new().chain_update(p).chain_update(secret).finalize();
        // клиент шифрует ключом, который сервер потом выведет как «dec»
        let mut enc = ctr(&key(&pk_iv[..32]), &pk_iv[32..]);
        let mut plain = hs.clone();
        plain[56..60].copy_from_slice(&proto);
        plain[60..62].copy_from_slice(&dc_idx.to_le_bytes());
        let mut full = plain.clone();
        enc.apply_keystream(&mut full);
        // в сети: первые 56 байт открытые, хвост шифрованный
        let mut wire = hs.clone();
        wire[56..].copy_from_slice(&full[56..]);
        // клиент для приёма использует развёрнутые ключи
        let rev: Vec<u8> = pk_iv.iter().rev().copied().collect();
        let dec = ctr(&key(&rev[..32]), &rev[32..]);
        (wire, enc, dec)
    }

    #[test]
    fn handshake_roundtrip() {
        let secret = random(16);
        for (proto, idx) in [(TAG_ABRIDGED, 2i16), (TAG_INTERMEDIATE, -4), (TAG_SECURE, 10002)] {
            let (wire, _, _) = client_hello(&secret, proto, idx);
            let hs = parse_handshake(&wire, &secret).expect("рукопожатие");
            assert_eq!(hs.proto, proto);
            assert_eq!(hs.media, idx < 0);
            assert_eq!(hs.test, idx >= 10000);
            assert_eq!(hs.dc as i32, (idx as i32).abs() - if idx >= 10000 { 10000 } else { 0 });
        }
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let (wire, _, _) = client_hello(&random(16), TAG_ABRIDGED, 2);
        assert!(parse_handshake(&wire, &random(16)).is_none());
    }

    #[test]
    fn relay_init_is_valid_for_telegram() {
        // Telegram (наш канал) расшифровывает 64 байта ключом из байт 8..56
        let r = relay_init(TAG_ABRIDGED, -2);
        assert_eq!(r.len(), 64);
        let mut d = r.clone();
        ctr(&r[8..40], &r[40..56]).apply_keystream(&mut d);
        assert_eq!(&d[56..60], &TAG_ABRIDGED);
        assert_eq!(i16::from_le_bytes([d[60], d[61]]), -2);
        assert_ne!(r[0], 0xef);
    }

    #[test]
    fn data_goes_client_to_telegram_and_back() {
        let secret = random(16);
        let (wire, mut cl_enc, mut cl_dec) = client_hello(&secret, TAG_ABRIDGED, 2);
        let hs = parse_handshake(&wire, &secret).unwrap();
        let relay = relay_init(hs.proto, 2);
        let mut cr = Crypto::new(&hs, &secret, &relay);

        // клиентский шифр уже прошёл 64 байта рукопожатия
        let msg = b"hello telegram, this is mtproto".to_vec();
        let mut on_wire = msg.clone();
        cl_enc.apply_keystream(&mut on_wire);

        // сервер: расшифровал клиента и зашифровал для Telegram
        let mut plain = on_wire.clone();
        cr.clt_dec.apply_keystream(&mut plain);
        assert_eq!(plain, msg);
        let mut to_tg = plain.clone();
        cr.tg_enc.apply_keystream(&mut to_tg);

        // «Telegram» читает тем же ключом, что описан в relay_init (после 64 байт)
        let mut tg_side = ctr(&relay[8..40], &relay[40..56]);
        tg_side.apply_keystream(&mut [0u8; 64]);
        let mut got = to_tg.clone();
        tg_side.apply_keystream(&mut got);
        assert_eq!(got, msg);

        // обратный путь: Telegram шифрует ответ своим «обратным» ключом
        let rr: Vec<u8> = relay[8..56].iter().rev().copied().collect();
        let mut tg_out = ctr(&rr[..32], &rr[32..]);
        let reply = b"reply from dc".to_vec();
        let mut from_tg = reply.clone();
        tg_out.apply_keystream(&mut from_tg);
        cr.tg_dec.apply_keystream(&mut from_tg);
        cr.clt_enc.apply_keystream(&mut from_tg);
        cl_dec.apply_keystream(&mut from_tg);
        assert_eq!(from_tg, reply);
    }

    #[test]
    fn splitter_abridged_cuts_packets() {
        // пакет 1: заголовок 2 -> 8 байт данных; пакет 2: заголовок 1 -> 4 байта
        let mut stream = vec![2u8];
        stream.extend_from_slice(&[7; 8]);
        stream.push(1);
        stream.extend_from_slice(&[9; 4]);
        let mut s = Splitter::new(TAG_ABRIDGED);
        // кормим кусками: сначала неполный пакет
        assert!(s.split(&stream[..5], &stream[..5]).is_empty());
        let parts = s.split(&stream[5..], &stream[5..]);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 9);
        assert_eq!(parts[1].len(), 5);
    }

    #[test]
    fn splitter_intermediate_and_unknown() {
        let mut stream = 8u32.to_le_bytes().to_vec();
        stream.extend_from_slice(&[1; 8]);
        let mut s = Splitter::new(TAG_INTERMEDIATE);
        assert_eq!(s.split(&stream, &stream).len(), 1);
        // нулевая длина отключает нарезку, дальше всё идёт как есть
        let zero = vec![0u8; 8];
        assert_eq!(s.split(&zero, &zero).len(), 1);
        assert_eq!(s.split(&[5, 5, 5], &[5, 5, 5]), vec![vec![5, 5, 5]]);
    }

    #[test]
    fn ws_frame_roundtrip() {
        for n in [0usize, 5, 125, 126, 300, 70000] {
            let data: Vec<u8> = (0..n).map(|i| i as u8).collect();
            let f = ws_frame(2, &data, [1, 2, 3, 4]);
            let (op, payload, fin, used) = ws_parse(&f).unwrap();
            assert_eq!((op, fin, used), (2, true, f.len()));
            assert_eq!(payload, data);
            if n > 0 {
                assert!(ws_parse(&f[..f.len() - 1]).is_none());
            }
        }
    }

    #[test]
    fn server_frames_without_mask() {
        let f = [0x82u8, 3, 1, 2, 3];
        assert_eq!(ws_parse(&f).unwrap().1, vec![1, 2, 3]);
    }

    #[test]
    fn link_and_secret() {
        let s = new_secret();
        assert_eq!(s.len(), 32);
        assert_eq!(unhex(&s).unwrap().len(), 16);
        assert_eq!(link(&s), format!("tg://proxy?server=127.0.0.1&port=1443&secret=dd{s}"));
    }
}
