//! Автообновление: берёт последний релиз с GitHub, скачивает nyxar.exe,
//! проверяет размер и sha256, подменяет запущенный exe и перезапускает программу.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

const REPO: &str = "Fersiya/wasd";
const ASSET: &str = "nyxar.exe";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    url: String,
    size: u64,
    sha256: Option<String>,
}

fn parse_version(s: &str) -> Vec<u32> {
    s.trim()
        .trim_start_matches('v')
        .split(|c| c == '.' || c == '-')
        .map_while(|p| p.parse().ok())
        .collect()
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    parse_version(remote) > parse_version(local)
}

/// Разбирает ответ GitHub API. `None`, если версия не новее или нет нужного файла.
fn parse_release(json: &str, current: &str) -> Result<Option<Release>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("Ответ GitHub не читается: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("В ответе GitHub нет версии")?;
    if !is_newer(tag, current) {
        return Ok(None);
    }
    let asset = v["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"] == ASSET))
        .ok_or_else(|| format!("В релизе {tag} нет файла {ASSET}"))?;
    Ok(Some(Release {
        version: tag.trim_start_matches('v').to_string(),
        url: asset["browser_download_url"]
            .as_str()
            .ok_or("В релизе нет ссылки на загрузку")?
            .to_string(),
        size: asset["size"].as_u64().unwrap_or(0),
        sha256: asset["digest"]
            .as_str()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(|d| d.to_lowercase()),
    }))
}

/// Спрашивает у GitHub последний релиз.
pub fn check() -> Result<Option<Release>, String> {
    let body = ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .set("User-Agent", "nyxar-updater")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .call()
        .map_err(|e| format!("Не удалось проверить обновления: {e}"))?
        .into_string()
        .map_err(|e| format!("Не удалось прочитать ответ GitHub: {e}"))?;
    parse_release(&body, env!("CARGO_PKG_VERSION"))
}

fn paths() -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Не найден путь программы: {e}"))?;
    let mut new = exe.clone().into_os_string();
    new.push(".new");
    let mut old = exe.clone().into_os_string();
    old.push(".old");
    Ok((exe, new.into(), old.into()))
}

fn download(rel: &Release, to: &PathBuf) -> Result<(), String> {
    let resp = ureq::get(&rel.url)
        .set("User-Agent", "nyxar-updater")
        .timeout(Duration::from_secs(900))
        .call()
        .map_err(|e| format!("Не удалось скачать обновление: {e}"))?;
    let mut reader = resp.into_reader();
    let mut file = fs::File::create(to).map_err(|e| format!("Не создался файл обновления: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("Загрузка прервалась: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| format!("Не записался файл обновления: {e}"))?;
        total += n as u64;
    }
    drop(file);

    if rel.size > 0 && total != rel.size {
        return Err(format!("Файл скачался не полностью ({total} из {} байт)", rel.size));
    }
    if let Some(want) = &rel.sha256 {
        let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if &got != want {
            return Err("Контрольная сумма обновления не совпала, файл отброшен".into());
        }
    }
    Ok(())
}

/// Скачивает обновление и подменяет текущий exe (запущенный exe можно переименовать).
pub fn install(rel: &Release) -> Result<(), String> {
    let (exe, new, old) = paths()?;
    let _ = fs::remove_file(&new);
    if let Err(e) = download(rel, &new) {
        let _ = fs::remove_file(&new);
        return Err(e);
    }
    let _ = fs::remove_file(&old);
    fs::rename(&exe, &old).map_err(|e| {
        let _ = fs::remove_file(&new);
        format!("Не удалось заменить программу (нет прав на запись в папку?): {e}")
    })?;
    if let Err(e) = fs::rename(&new, &exe) {
        let _ = fs::rename(&old, &exe); // откат
        return Err(format!("Не удалось поставить новую версию: {e}"));
    }
    Ok(())
}

/// Запускает новую версию после выхода текущей (иначе она увидит занятый порт и закроется).
pub fn relaunch() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new("cmd")
                .raw_arg(format!(
                    "/C ping 127.0.0.1 -n 3 >nul & start \"\" \"{}\"",
                    exe.display()
                ))
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .spawn();
        }
    }
}

/// Удаляет хвосты прошлых обновлений.
pub fn cleanup() {
    if let Ok((_, new, old)) = paths() {
        let _ = fs::remove_file(old);
        let _ = fs::remove_file(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.6.0", "0.5.0"));
        assert!(is_newer("0.5.1", "0.5.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.5.0", "0.5.0"));
        assert!(!is_newer("0.4.9", "0.5.0"));
        assert!(is_newer("0.10.0", "0.9.0"));
    }

    const JSON: &str = r#"{
        "tag_name": "v0.6.0",
        "assets": [
            {"name": "Nyxar-Setup-0.6.0.exe", "browser_download_url": "https://x/setup.exe", "size": 5},
            {"name": "nyxar.exe", "browser_download_url": "https://x/nyxar.exe", "size": 1234, "digest": "sha256:ABCDEF"}
        ]
    }"#;

    #[test]
    fn parses_newer_release() {
        let r = parse_release(JSON, "0.5.0").unwrap().unwrap();
        assert_eq!(r.version, "0.6.0");
        assert_eq!(r.url, "https://x/nyxar.exe");
        assert_eq!(r.size, 1234);
        assert_eq!(r.sha256.as_deref(), Some("abcdef"));
    }

    #[test]
    fn ignores_same_or_older_release() {
        assert!(parse_release(JSON, "0.6.0").unwrap().is_none());
        assert!(parse_release(JSON, "0.7.0").unwrap().is_none());
    }

    #[test]
    fn missing_asset_is_an_error() {
        let j = r#"{"tag_name":"v0.9.0","assets":[]}"#;
        assert!(parse_release(j, "0.5.0").is_err());
    }
}
