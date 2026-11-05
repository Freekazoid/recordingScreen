# Собирает Windows-комплект v4 и установщик NSIS.
# Требования: Rust GNU toolchain, MinGW (gcc/g++), NSIS (makensis), CMake.
param(
  [string]$Version = "4.0.0-alpha.1",
  [string]$Target = "x86_64-pc-windows-gnu"
)
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$dist = Join-Path $root "dist\windows"
$out = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist, $out | Out-Null

Push-Location $root
try {
  cargo build --release --target $Target -p screenrecorder-app -p screenrecorder-ml-worker -p screenrecorder-diarize
  $bin = Join-Path $root "target\$Target\release"
  Copy-Item (Join-Path $bin "screenrecorder.exe") $dist -Force
  Copy-Item (Join-Path $bin "screenrecorder-ml.exe") $dist -Force
  Copy-Item (Join-Path $bin "screenrecorder-diarize.exe") $dist -Force
  Copy-Item (Join-Path $root "assets\icon.ico") $dist -Force
  Get-ChildItem (Join-Path $root "target\sherpa-onnx-prebuilt") -Recurse -Filter "*.dll" |
    Where-Object { $_.Name -match "^(sherpa|onnxruntime)" } |
    ForEach-Object { Copy-Item $_.FullName $dist -Force }

  if (-not (Test-Path (Join-Path $dist "ffmpeg.exe"))) {
    $zip = Join-Path $env:TEMP "ffmpeg-win.zip"
    Invoke-WebRequest "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip" -OutFile $zip
    $tmp = Join-Path $env:TEMP "ffmpeg-win"
    Expand-Archive $zip $tmp -Force
    Get-ChildItem $tmp -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1 | ForEach-Object { Copy-Item $_.FullName $dist -Force }
    Get-ChildItem $tmp -Recurse -Filter "ffprobe.exe" | Select-Object -First 1 | ForEach-Object { Copy-Item $_.FullName $dist -Force }
  }

  @"
Screen Recorder 4.0 (Windows)
=============================

Запуск: screenrecorder.exe

При первом скачивании моделей приложение попросит выбрать папку.
Модели сохраняются в <выбранная папка>\models. Если модели уже есть,
укажите их папку — приложение найдёт их и докачает только недостающие.

В комплекте: ffmpeg.exe, ffprobe.exe, screenrecorder-ml.exe (Whisper),
screenrecorder-diarize.exe (sherpa-onnx) и DLL sherpa/onnxruntime.
"@ | Set-Content (Join-Path $dist "README-win.txt") -Encoding UTF8

  & makensis /DAPP_VERSION=$Version /DPAYLOAD="$dist" /DOUTDIR="$out" (Join-Path $PSScriptRoot "installer.nsi")
  Write-Host "Готово: $out"
} finally {
  Pop-Location
}
