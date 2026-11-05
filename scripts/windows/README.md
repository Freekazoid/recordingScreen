# Сборка Windows v4

Требования: Rust, CMake, C/C++ toolchain (MSVC или MinGW GNU), NSIS 3.

```powershell
# из корня репозитория
powershell -ExecutionPolicy Bypass -File scripts/windows/build.ps1 -Version 4.0.0-alpha.1
```

Скрипт собирает `screenrecorder.exe`, `screenrecorder-ml.exe`,
`screenrecorder-diarize.exe`, копирует sherpa/onnxruntime DLL и ffmpeg,
затем собирает per-user установщик в `dist/`.

Для GNU-кросс-сборки на Linux нужны LLVM-MinGW и шимы libgcc/libstdc++;
пример окружения:
`RUSTFLAGS="-L /tmp/opencode/winlibs-shim -L /tmp/opencode/whisper-shim -l advapi32"`.
