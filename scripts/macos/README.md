# Сборка macOS v4

Сборка выполняется только на macOS (нужен SDK и `codesign`), как и v3.

## Быстрый старт

```bash
# подготовка
rustup target add aarch64-apple-darwin x86_64-apple-darwin
# (для ffmpeg/ASR нужны CMake и clang из Xcode Command Line Tools)

# Apple Silicon
MACOS_TARGET_ARCH=arm64 APP_VERSION=4.0.0-alpha.3 bash scripts/macos/build.sh

# Universal2 (собирает оба target и склеивает lipo)
MACOS_TARGET_ARCH=universal2 APP_VERSION=4.0.0-alpha.3 bash scripts/macos/build.sh

# опционально DMG и подпись
CREATE_DMG=1 MACOS_SIGN_IDENTITY="Developer ID Application: ..." \
  bash scripts/macos/build.sh
```

Результат: `dist/ScreenRecorder.app`, zip, при `CREATE_DMG=1` — dmg.

## Права macOS

При первом запуске разрешите:
- **Screen Recording / Screen & System Audio Recording** — для `avfoundation`;
- **Microphone** — для микрофона.

`Info.plist` содержит `NSScreenCaptureUsageDescription`,
`NSMicrophoneUsageDescription`, `NSAudioCaptureUsageDescription`.
Ad-hoc подпись (`-`) годится для локальной проверки; для распространения
нужна Developer ID и notarization (`MACOS_NOTARY_PROFILE`).

## Системный звук

FFmpeg `avfoundation` не умеет писать системный звук без виртуального
loopback-устройства. Установите **BlackHole** (или аналог) и выберите его
в настройках приложения как «Системный звук». Без него запись идёт с
микрофона, а приложение показывает предупреждение — как v3.

Native ScreenCaptureKit (системный звук без BlackHole) — следующий этап,
не входит в MVP.

## Ограничения MVP

- режим «Программа» на macOS пока не выбирает окно (нужен CGWindowList);
- звук системный — через BlackHole;
- для universal2 нужен универсальный/оба ffmpeg; скрипт скачивает arm64+x64
  статические сборки и склеивает их `lipo`.
