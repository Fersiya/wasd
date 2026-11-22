//! Экономия трафика: блокировка рекламы и трекеров на уровне VPN, учёт трафика по программам
//! и месячный лимит.
//!
//! Блокировка работает дважды: DNS-запрос к такому домену получает отказ (соединение даже не
//! начинается), а если адрес всё же известен, соединение сбрасывается правилом маршрутизации.

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, path::Path};

/// Домены рекламных сетей и трекеров. Список короткий и намеренно осторожный: только то,
/// что не нужно для работы сайтов и приложений.
pub const ADS: &[&str] = &[
    "doubleclick.net", "googlesyndication.com", "googleadservices.com", "adservice.google.com",
    "pagead2.googlesyndication.com", "google-analytics.com", "googletagmanager.com", "googletagservices.com",
    "admob.com", "adsense.com", "adnxs.com", "adsrvr.org", "adform.net", "adition.com", "advertising.com",
    "criteo.com", "criteo.net", "taboola.com", "outbrain.com", "revcontent.com", "mgid.com", "pubmatic.com",
    "rubiconproject.com", "openx.net", "casalemedia.com", "indexww.com", "smartadserver.com", "bidswitch.net",
    "contextweb.com", "amazon-adsystem.com", "media.net", "yieldmo.com", "sharethrough.com", "teads.tv",
    "scorecardresearch.com", "quantserve.com", "quantcount.com", "chartbeat.com", "chartbeat.net",
    "hotjar.com", "mouseflow.com", "fullstory.com", "crazyegg.com", "inspectlet.com", "luckyorange.com",
    "mixpanel.com", "segment.io", "segment.com", "amplitude.com", "heap.io", "heapanalytics.com",
    "branch.io", "adjust.com", "adjust.io", "appsflyer.com", "kochava.com", "singular.net", "tenjin.io",
    "app-measurement.com", "firebase-settings.crashlytics.com", "crashlytics.com", "flurry.com",
    "unityads.unity3d.com", "applovin.com", "applvn.com", "ironsrc.com", "supersonicads.com",
    "vungle.com", "chartboost.com", "inmobi.com", "mopub.com", "startapp.com", "adcolony.com", "tapjoy.com",
    "moatads.com", "adsafeprotected.com", "doubleverify.com", "serving-sys.com", "2mdn.net",
    "an.facebook.com", "analytics.tiktok.com", "ads.tiktok.com",
    "business-api.tiktok.com", "log.byteoversea.com", "ads.twitter.com", "static.ads-twitter.com",
    "analytics.twitter.com", "ads.linkedin.com", "px.ads.linkedin.com", "snap.licdn.com", "ads.pinterest.com",
    "ads.yahoo.com", "analytics.yahoo.com", "ads.reddit.com", "events.redditmedia.com",
    "mc.yandex.ru", "mc.yandex.com", "an.yandex.ru", "yandexadexchange.net", "adfox.yandex.ru",
    "ads.adfox.ru", "top-fwz1.mail.ru", "counter.yadro.ru", "mediametrics.ru", "adriver.ru",
    "ad.mail.ru", "tns-counter.ru", "adhigh.net", "buzzoola.com", "otm-r.com", "bidvol.com",
    "zedo.com", "bluekai.com", "demdex.net", "everesttech.net", "krxd.net", "exelator.com", "rlcdn.com",
    "tapad.com", "mathtag.com", "agkn.com", "addthis.com", "sharethis.com",     "hs-analytics.net", "hs-scripts.com", "optimizely.com", "newrelic.com", "nr-data.net", "bugsnag.com",
    "sentry.io", "datadoghq.com", "statcounter.com", "histats.com", "clicky.com",
];

/// Месяц вида "2026-11" по времени UNIX (UTC).
pub fn month_key(secs: u64) -> String {
    let z = (secs / 86400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}")
}

pub fn now_month() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    month_key(secs)
}

/// Короткое имя программы из пути или пакета: `C:\\x\\chrome.exe` → `chrome.exe`.
pub fn short_name(path: &str) -> String {
    let p = path.trim();
    // Android: «com.app (10123)» — берём только пакет
    let p = p.split(" (").next().unwrap_or(p);
    let name = p.rsplit(['\\', '/']).next().unwrap_or(p).trim();
    name.to_string()
}

/// Одно соединение из API ядра.
#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub id: String,
    pub process: String,
    pub up: u64,
    pub down: u64,
}

/// Учёт трафика за месяц: всего и по программам.
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Usage {
    #[serde(default)]
    pub month: String,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub apps: HashMap<String, u64>,
    /// Какое предупреждение о лимите уже показано в этом месяце: 0 нет, 1 — 80%, 2 — 100%
    #[serde(default)]
    pub warned: u8,
    #[serde(skip)]
    seen: HashMap<String, (u64, u64)>,
}

impl Usage {
    pub fn load(dir: &Path) -> Usage {
        fs::read_to_string(dir.join("usage.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) {
        if let Ok(s) = serde_json::to_string(self) {
            let tmp = dir.join("usage.json.tmp");
            if fs::write(&tmp, s).is_ok() {
                let _ = fs::rename(&tmp, dir.join("usage.json"));
            }
        }
    }

    /// Новое подключение: счётчики соединений начинаются заново.
    pub fn new_session(&mut self) {
        self.seen.clear();
    }

    pub fn reset(&mut self) {
        let month = std::mem::take(&mut self.month);
        *self = Usage::default();
        self.month = month;
    }

    /// Учитывает свежий список соединений. Возвращает, сколько байт добавилось.
    pub fn feed(&mut self, month: &str, conns: &[Conn]) -> u64 {
        if self.month != month {
            self.month = month.to_string();
            self.total = 0;
            self.apps.clear();
            self.warned = 0;
        }
        let mut added = 0u64;
        let mut now: HashMap<String, (u64, u64)> = HashMap::with_capacity(conns.len());
        for c in conns {
            let (pu, pd) = self.seen.get(&c.id).copied().unwrap_or((0, 0));
            let delta = c.up.saturating_sub(pu) + c.down.saturating_sub(pd);
            if delta > 0 {
                added += delta;
                let name = if c.process.is_empty() { "—".to_string() } else { short_name(&c.process) };
                *self.apps.entry(name).or_insert(0) += delta;
            }
            now.insert(c.id.clone(), (c.up, c.down));
        }
        self.seen = now;
        self.total += added;
        added
    }

    /// Самые прожорливые программы: (имя, байты).
    pub fn top(&self, n: usize) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self.apps.iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    /// Достигнут ли новый порог лимита (в МБ). Каждый порог срабатывает один раз за месяц.
    pub fn check_limit(&mut self, limit_mb: u64) -> Option<u8> {
        if limit_mb == 0 {
            return None;
        }
        let limit = limit_mb.saturating_mul(1024 * 1024);
        let level = if self.total >= limit {
            2
        } else if self.total.saturating_mul(5) >= limit.saturating_mul(4) {
            1
        } else {
            0
        };
        if level > self.warned {
            self.warned = level;
            Some(level)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, p: &str, up: u64, down: u64) -> Conn {
        Conn { id: id.into(), process: p.into(), up, down }
    }

    #[test]
    fn month_from_epoch() {
        assert_eq!(month_key(0), "1970-01");
        assert_eq!(month_key(1_795_000_000), "2026-11"); // 2026-11-18
        assert_eq!(month_key(1_709_164_800), "2024-02"); // 29 февраля високосного года
        assert_eq!(month_key(1_709_251_200), "2024-03");
    }

    #[test]
    fn names_are_short() {
        assert_eq!(short_name(r"C:\Program Files\Google\Chrome\chrome.exe"), "chrome.exe");
        assert_eq!(short_name("/usr/bin/curl"), "curl");
        assert_eq!(short_name("org.telegram.messenger (10123)"), "org.telegram.messenger");
        assert_eq!(short_name("123"), "123");
    }

    #[test]
    fn feed_counts_only_growth() {
        let mut u = Usage::default();
        assert_eq!(u.feed("2026-11", &[c("a", "x.exe", 100, 900)]), 1000);
        assert_eq!(u.feed("2026-11", &[c("a", "x.exe", 150, 1900)]), 1050);
        // соединение закрылось, появилось новое
        assert_eq!(u.feed("2026-11", &[c("b", "y.exe", 10, 20)]), 30);
        assert_eq!(u.total, 2080);
        assert_eq!(u.top(5), vec![("x.exe".to_string(), 2050), ("y.exe".to_string(), 30)]);
    }

    #[test]
    fn new_month_resets() {
        let mut u = Usage::default();
        u.feed("2026-11", &[c("a", "x.exe", 1, 1)]);
        u.warned = 2;
        u.feed("2026-12", &[c("b", "x.exe", 5, 5)]);
        assert_eq!(u.total, 10);
        assert_eq!(u.warned, 0);
        assert_eq!(u.month, "2026-12");
    }

    #[test]
    fn limit_fires_once_per_level() {
        let mut u = Usage::default();
        u.total = 100 * 1024 * 1024;
        assert_eq!(u.check_limit(0), None);
        assert_eq!(u.check_limit(1000), None);
        u.total = 850 * 1024 * 1024;
        assert_eq!(u.check_limit(1000), Some(1));
        assert_eq!(u.check_limit(1000), None);
        u.total = 1000 * 1024 * 1024;
        assert_eq!(u.check_limit(1000), Some(2));
        assert_eq!(u.check_limit(1000), None);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("nyxar-usage-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut u = Usage::default();
        u.feed("2026-11", &[c("a", "x.exe", 7, 3)]);
        u.save(&dir);
        let l = Usage::load(&dir);
        assert_eq!(l.total, 10);
        assert_eq!(l.top(1)[0].0, "x.exe");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ad_list_is_clean() {
        let mut seen = std::collections::HashSet::new();
        for d in ADS {
            assert!(d.contains('.') && !d.contains(' ') && !d.contains('/'), "{d}");
            assert!(seen.insert(*d), "дубль {d}");
        }
    }
}
