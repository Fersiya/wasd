@echo off
chcp 65001 >nul
title Сборка и подготовка Rust Light VPN
echo ===================================================
echo   1. Проверка и скачивание ядра sing-box.exe...
echo ===================================================

if not exist "sing-box.exe" (
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
      "$rel = Invoke-RestMethod -Uri 'https://api.github.com/repos/SagerNet/sing-box/releases/latest';" ^
      "$asset = $rel.assets | Where-Object { $_.name -like '*windows-amd64.zip' } | Select-Object -First 1;" ^
      "Write-Host 'Скачивание' $asset.name '...';" ^
      "Invoke-WebRequest -Uri $asset.browser_download_url -OutFile 'sb.zip';" ^
      "Expand-Archive -Path 'sb.zip' -DestinationPath 'sb_tmp' -Force;" ^
      "$exe = Get-ChildItem -Path 'sb_tmp' -Filter 'sing-box.exe' -Recurse | Select-Object -First 1;" ^
      "Copy-Item $exe.FullName -Destination '.\sing-box.exe' -Force;" ^
      "Remove-Item 'sb.zip' -Force; Remove-Item 'sb_tmp' -Recurse -Force;"
) else (
    echo sing-box.exe уже существует, пропускаем скачивание.
)

echo.
echo ===================================================
echo   2. Компиляция ультралегкого rust-light-vpn.exe...
echo ===================================================
cargo build --release

if exist "target\release\rust-light-vpn.exe" (
    copy /Y "target\release\rust-light-vpn.exe" ".\rust-light-vpn.exe" >nul
    echo.
    echo [ГОТОВО!] Файлы rust-light-vpn.exe и sing-box.exe готовы к работе!
    echo Запускаю клиент...
    start "" ".\rust-light-vpn.exe"
) else (
    echo.
    echo [ОШИБКА] Не удалось собрать проект. Убедитесь, что установлен Rust (rustup.rs).
)
pause
