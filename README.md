<div align="center">

# Rust Light VPN

**Лёгкий VPN-клиент для Windows на Rust: живёт в трее, почти не ест память**

[![build](https://github.com/Fersiya/wasd/actions/workflows/build.yml/badge.svg)](https://github.com/Fersiya/wasd/actions/workflows/build.yml)
![platform](https://img.shields.io/badge/platform-Windows%2010%2F11-0078D4)
![rust](https://img.shields.io/badge/Rust-2021-orange)

</div>

---

## Возможности

- **Трей вместо окна.** Подключение, отключение и настройки в одном меню.
- **Минимум ресурсов.** Сам клиент занимает около 1 МБ на диске и несколько МБ в памяти.
- **VLESS из коробки.** Принимает ключ `vless://` или ссылку на подписку (Reality, TLS, WebSocket, gRPC).
- **Системный TUN.** Весь трафик идёт через туннель, без настройки прокси в каждой программе.
- **Простые настройки.** Ссылка вводится в окне настроек и сохраняется рядом с программой.

## Установка

Скачай готовую сборку из вкладки **Actions** → последний успешный запуск → артефакт `rust-light-vpn-windows-x64`, распакуй в любую папку.

В архиве три файла, держи их вместе:

| Файл | Назначение |
|---|---|
| `rust-light-vpn.exe` | сам клиент |
| `sing-box.exe` | сетевое ядро |
| `wintun.dll` | драйвер TUN-адаптера |

## Использование

1. Запусти `rust-light-vpn.exe`. Windows запросит права администратора, они нужны для создания TUN-адаптера.
2. Нажми на иконку в трее → **Настройки…** → вставь ключ или ссылку на подписку → **Сохранить и подключить**.
3. Иконка станет зелёной, когда соединение установлено. Отключение через то же меню.

## Сборка из исходников

Нужен [Rust](https://rustup.rs) и Windows.

```bat
build_and_run.bat
```

Скрипт скачает ядро, соберёт клиент и сразу запустит его.

Либо вручную:

```bat
cargo build --release
```

Готовый файл: `target\release\rust-light-vpn.exe`. Положи рядом `sing-box.exe` и `wintun.dll`.

## Файлы рядом с программой

| Файл | Что внутри |
|---|---|
| `settings.json` | сохранённая ссылка |
| `config.json` | сгенерированный конфиг ядра |
| `sing-box.log` | лог ядра, смотри сюда при проблемах |

## Стек

[`tao`](https://github.com/tauri-apps/tao) · [`wry`](https://github.com/tauri-apps/wry) · [`tray-icon`](https://github.com/tauri-apps/tray-icon) · [`sing-box`](https://github.com/SagerNet/sing-box)
