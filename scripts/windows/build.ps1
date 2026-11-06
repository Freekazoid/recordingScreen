# Собирает Windows-комплект v4 и установщик NSIS.
# Требования: Rust GNU toolchain, MinGW (gcc/g++), NSIS (makensis), CMake.
param(
  [string]$Version = "4.0.0-alpha.3",
  [string]$Target = "x86_64-pc-windows-gnu"
)
$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$dist = Join-Path $root "dist\windows"
$out = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist, $out | Out-Null
$log = Join-Path $out "build-windows.log"
function Fail-Message([string]$message) {
  $escaped = $message.Replace("%", "%25").Replace("`r", "%0D").Replace("`n", "%0A")
  Write-Output "::error::$escaped"
}
trap {
  Fail-Message $_.Exception.Message
  exit 1
}

Push-Location $root
try {
  # Кэш нативных библиотек sherpa мог сохраниться от другой разрядности/режима.
  if (Test-Path (Join-Path $root "target\sherpa-onnx-prebuilt")) {
    Remove-Item (Join-Path $root "target\sherpa-onnx-prebuilt") -Recurse -Force
  }
  $cargo = (Get-Command cargo).Source
  $cargoArgs = @(
    "build", "--release", "--target", $Target,
    "-p", "screenrecorder-app", "-p", "screenrecorder-ml-worker",
    "-p", "screenrecorder-diarize"
  )
  $cargoErr = "$log.err"
  $process = Start-Process -FilePath $cargo -ArgumentList $cargoArgs -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput $log -RedirectStandardError $cargoErr
  if (Test-Path $cargoErr) { Get-Content $cargoErr | Out-File -Append $log; Remove-Item $cargoErr -Force }
  if ($process.ExitCode -ne 0) {
    $tail = (Get-Content $log -Tail 25) -join " | "
    throw "cargo build failed ($($process.ExitCode)): $tail"
  }
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

  $portable = Join-Path $out "ScreenRecorder-$Version-win64-portable.zip"
  if (Test-Path $portable) { Remove-Item $portable -Force }
  Compress-Archive -Path (Join-Path $dist "*") -DestinationPath $portable
  $base = ($Version -split '-')[0]
  $parts = $base.Split('.')
  while ($parts.Count -lt 3) { $parts += '0' }
  $suffix = if ($Version -match '\.(\d+)$') { $Matches[1] } else { '0' }
  $numericVersion = "$($parts[0]).$($parts[1]).$($parts[2]).$suffix"
  $makensis = (Get-Command makensis).Source
  $installer = Join-Path $PSScriptRoot "installer.nsi"
  $nsisErr = "$log.nsis.err"
  $process = Start-Process -FilePath $makensis -ArgumentList @(
    "/DAPP_VERSION=$Version", "/DAPP_VERSION_NUMERIC=$numericVersion",
    "/DPAYLOAD=$dist", "/DOUTDIR=$out", $installer
  ) -NoNewWindow -Wait -PassThru `
    -RedirectStandardOutput "$log.nsis" -RedirectStandardError $nsisErr
  Get-Content "$log.nsis" | Out-File -Append $log
  if (Test-Path $nsisErr) { Get-Content $nsisErr | Out-File -Append $log; Remove-Item $nsisErr -Force }
  Remove-Item "$log.nsis" -Force -ErrorAction SilentlyContinue
  if ($process.ExitCode -ne 0) {
    $tail = (Get-Content $log -Tail 25) -join " | "
    throw "makensis failed ($($process.ExitCode)): $tail"
  }
  Write-Host "Готово: $out"
} finally {
  Pop-Location
}
