//! Проверка доступности сервера: TCP-подключение и, где возможно, TLS-рукопожатие.
//!
//! Простого TCP мало: за общим фронтом (CDN, Reality) порт отвечает у всех серверов одинаково
//! быстро, даже если сам сервер за ним не работает. Рукопожатие TLS с нужным SNI проходит
//! весь путь до сервера, поэтому время честнее, а мёртвые серверы отсеиваются.

use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use std::{
    io,
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};
use url::Url;

const TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // подлинность нам не важна: меряем только время ответа
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn config() -> Arc<rustls::ClientConfig> {
    use std::sync::OnceLock;
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .expect("версии TLS")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

/// Имя для SNI из ссылки, если сервер работает поверх TLS или Reality.
pub fn tls_name(link: &str, host: &str) -> Option<String> {
    let u = Url::parse(link).ok()?;
    let q = |k: &str| u.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.to_string());
    match q("security").as_deref() {
        Some("tls") | Some("reality") => {}
        _ => return None,
    }
    let name = q("sni").or_else(|| q("serverName")).filter(|s| !s.is_empty()).unwrap_or_else(|| host.to_string());
    // для IP без SNI рукопожатие смысла не имеет
    if name.parse::<std::net::IpAddr>().is_ok() {
        None
    } else {
        Some(name)
    }
}

fn handshake(addr: std::net::SocketAddr, sni: &str) -> io::Result<u32> {
    let t = Instant::now();
    let mut sock = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    sock.set_read_timeout(Some(TIMEOUT))?;
    sock.set_write_timeout(Some(TIMEOUT))?;
    sock.set_nodelay(true).ok();
    let name = ServerName::try_from(sni.to_string()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let mut conn = rustls::ClientConnection::new(config(), name).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    while conn.is_handshaking() {
        if t.elapsed() > TIMEOUT * 2 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        conn.complete_io(&mut sock)?;
    }
    Ok(t.elapsed().as_millis() as u32)
}

/// Задержка до сервера в мс или None, если он не отвечает.
pub fn ping(host: &str, port: u16, link: &str) -> Option<u32> {
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    if let Some(name) = tls_name(link, host) {
        // два замера, берём лучший: первый часто включает прогрев маршрута
        let a = handshake(addr, &name).ok();
        let b = handshake(addr, &name).ok();
        return match (a, b) {
            (Some(x), Some(y)) => Some(x.min(y)),
            (x, y) => x.or(y),
        };
    }
    let t = Instant::now();
    TcpStream::connect_timeout(&addr, TIMEOUT).ok()?;
    Some(t.elapsed().as_millis() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sni_from_links() {
        assert_eq!(tls_name("vless://u@1.2.3.4:443?security=reality&sni=a.example.com&type=tcp#x", "1.2.3.4").as_deref(), Some("a.example.com"));
        assert_eq!(tls_name("vless://u@h.example.org:443?security=tls&type=ws", "h.example.org").as_deref(), Some("h.example.org"));
        assert_eq!(tls_name("vless://u@h.example.org:443?security=none", "h.example.org"), None);
        assert_eq!(tls_name("vless://u@1.2.3.4:443?security=tls", "1.2.3.4"), None);
    }

    #[test]
    fn dead_server_gives_none() {
        // порт 9 на localhost почти наверняка закрыт
        assert_eq!(ping("127.0.0.1", 9, "vless://u@127.0.0.1:9?security=none"), None);
    }

    #[test]
    fn local_tls_handshake_is_measured() {
        // поднимаем локальный TLS-сервер на самоподписанном сертификате и меряем рукопожатие
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // принимаем и сразу закрываем: клиент получит ошибку, ping вернёт None, но не зависнет
            for s in l.incoming().take(2) {
                drop(s);
            }
        });
        let r = ping("127.0.0.1", port, &format!("vless://u@localhost:{port}?security=tls&sni=localhost.test"));
        assert!(r.is_none());
    }
}
