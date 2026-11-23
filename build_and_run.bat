@echo off
chcp 65001 >nul
title Сборка и подготовка Nyxar
echo ===================================================
echo   1. Проверка и скачивание компонентов ядра...
echo ===================================================

if not exist "sing-box.exe" (
    echo Скачивание sing-box.exe...
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
      "$rel = Invoke-RestMethod -Uri 'https://api.github.com/repos/SagerNet/sing-box/releases/latest';" ^
      "$asset = $rel.assets | Where-Object { $_.name -like '*windows-amd64.zip' } | Select-Object -First 1;" ^
      "Invoke-WebRequest -Uri $asset.browser_download_url -OutFile 'sb.zip';" ^
      "Expand-Archive -Path 'sb.zip' -DestinationPath 'sb_tmp' -Force;" ^
      "$exe = Get-ChildItem -Path 'sb_tmp' -Filter 'sing-box.exe' -Recurse | Select-Object -First 1;" ^
      "Copy-Item $exe.FullName -Destination '.\sing-box.exe' -Force;" ^
      "Remove-Item 'sb.zip' -Force; Remove-Item 'sb_tmp' -Recurse -Force;"
) else (
    echo sing-box.exe уже существует.
)

if not exist "wintun.dll" (
    echo Скачивание wintun.dll для TUN-адаптера...
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
      "Invoke-WebRequest -Uri 'https://www.wintun.net/builds/wintun-0.14.1.zip' -OutFile 'wintun.zip';" ^
      "Expand-Archive -Path 'wintun.zip' -DestinationPath 'wintun_tmp' -Force;" ^
      "Copy-Item 'wintun_tmp\wintun\bin\amd64\wintun.dll' -Destination '.\wintun.dll' -Force;" ^
      "Remove-Item 'wintun.zip' -Force; Remove-Item 'wintun_tmp' -Recurse -Force;"
) else (
    echo wintun.dll уже существует.
)

if not exist "xray.exe" (
    echo Скачивание xray.exe для xhttp и VLESS-encryption...
    powershell -NoProfile -ExecutionPolicy Bypass -Command ^
      "$xr = Invoke-RestMethod -Uri 'https://api.github.com/repos/XTLS/Xray-core/releases/latest';" ^
      "$xa = $xr.assets | Where-Object { $_.name -eq 'Xray-windows-64.zip' } | Select-Object -First 1;" ^
      "Invoke-WebRequest -Uri $xa.browser_download_url -OutFile 'xray.zip';" ^
      "Expand-Archive -Path 'xray.zip' -DestinationPath 'xray_tmp' -Force;" ^
      "Copy-Item 'xray_tmp\xray.exe' -Destination '.\xray.exe' -Force;" ^
      "Remove-Item 'xray.zip' -Force; Remove-Item 'xray_tmp' -Recurse -Force;"
) else (
    echo xray.exe уже существует.
)

echo.
echo ===================================================
echo   2. Компиляция ультралегкого nyxar.exe...
echo ===================================================
cargo build --release

if exist "target\release\nyxar.exe" (
    copy /Y "target\release\nyxar.exe" ".\nyxar.exe" >nul
    echo.
    echo [ГОТОВО!] Все файлы готовы к работе!
    echo Запускаю клиент...
    start "" ".\nyxar.exe"
) else (
    echo.
    echo [ОШИБКА] Не удалось собрать проект. Убедитесь, что установлен Rust (rustup.rs).
)
pause
