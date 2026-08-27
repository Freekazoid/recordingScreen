# Screen Recorder 🎥 — Запись экрана с транскрипцией и диаризацией

> Полнофункциональное десктопное приложение для записи экрана в Linux с автоматическим распознаванием речи (ASR) и определением говорящих. Создано для ежедневных стендапов, рабочих встреч, вебинаров, лекций и всего, что нужно сохранить не только в видео, но и в тексте с указанием кто и когда говорил.

**Автор:** Freekazoid — [Telegram](https://t.me/Ifreekazoid)
**Исходный код:** [GitHub](https://github.com/Freekazoid/recordingScreen)
**Лицензия:** MIT
**Версия:** 3.2.194

---

## ✨ Возможности

### 🎬 Запись видео
- **Весь экран** — захват полного монитора через `mss` (20 FPS)
- **Область экрана** — интерактивное выделение области мышью (12 FPS)
- **Окно приложения** — выбор окна из списка с предпросмотром иконок, захват через Xlib
- **Только аудио** — запись без видео, только системный звук + микрофон

### 🎙 Запись аудио
- Одновременная запись **системного звука** (десктоп) и **микрофона**
- Поддержка PulseAudio и PipeWire на Linux, DirectShow/WASAPI на Windows
- Автоматическое смешивание дорожек

### 🤖 Транскрипция речи (Speech-to-Text)
- **WhisperX** (`faster-whisper large-v3-turbo`, OpenAI) — рекомендуется, быстрое и точное распознавание
- **Vosk** — лёгкая офлайн-альтернатива (без GPU)
- Результат: `.txt` файл с текстом (опционально с таймкодами)

### 🗣 Диаризация (определение говорящих)
- **PyAnnote** (`pyannote/speaker-diarization-3.1`) — нейросетевая диаризация, нужен HF_TOKEN
- **NeMo Sortformer** (`nvidia/diar_sortformer_4spk-v1`) — до 4 спикеров, без токена
- Результат: `.md` файл с размеченными спикерами:
  ```
  **Спикер 1** [00:12 - 00:28]: текст...
  **Спикер 2** [00:28 - 00:45]: текст...
  ```

### ⚙️ Пост-обработка
- Склейка видео + аудио в один файл (mp4, mkv, webm, avi, mov, flv)
- Настраиваемое качество видео (CRF 18/23/28/35)
- Режимы аудиодорожки: оригинал, сжатие (AAC 128k), удаление
- Асинхронная обработка в фоновом процессе — интерфейс не зависает

### 🧠 Управление моделями
- Скачивание моделей WhisperX, PyAnnote, NeMo Sortformer и Vosk прямо из интерфейса
- Визуальные индикаторы прогресса, подсказки «ⓘ» с описанием каждой модели
- HF_TOKEN нужен только PyAnnote

---

## 🚀 Быстрый старт

### Запуск из исходников (разработка)

```bash
# Клонирование
git clone https://github.com/Freekazoid/recordingScreen
cd recordingScreen

# Всё окружение одной командой (без sudo):
# CPython 3.14 в .python/, venv, зависимости и _tkinter с fontconfig
bash scripts/setup_env.sh

# Запуск
.venv/bin/python src/main.py
```

> ⚠️ **Важно про шрифты:** bundled-сборки Python (python-build-standalone, conda)
> поставляют Tk без fontconfig — кириллица рендерится на ~70% шире.
> `scripts/setup_env.sh` автоматически собирает `_tkinter` против системного
> Tcl/Tk 8.6 (Xft/fontconfig). `build_AppImage.sh` проверяет это и отклоняет
> сборку с кривым шрифтом. После переноса проекта просто запустите скрипт заново.

### Сборка standalone-билдов

```bash
# Linux AppImage
./build_AppImage.sh

# macOS .app + .zip + .dmg
./build_macOS.sh

# Windows .exe (кросс-компиляция через Docker)
./build_windows.sh
```

### 📦 GitHub-релизы: сборка AppImage из частей

Сборки с CUDA-поддержкой (GPU) весят **больше 2 ГБ**, поэтому GitHub не
загружает их одним файлом — лимит одного файла в релизе **2 ГБ**. Workflow
автоматически делит AppImage командой `split -b 1500M` на части:

```
ScreenRecorder_vbuild-48.AppImage.part-aa   (1.46 GB)
ScreenRecorder_vbuild-48.AppImage.part-ab   (1.46 GB)
ScreenRecorder_vbuild-48.AppImage.part-ac   (872 MB)
```

Чтобы восстановить целый AppImage, скачайте **все** части (одну и ту же
версию/сборку — не перемешивайте!), объедините их **по алфавиту** (`aa`, `ab`,
`ac`, …) и сделайте исполняемым:

```bash
cat ./*.AppImage.part-* > ScreenRecorder_vbuild-48.AppImage
chmod +x ScreenRecorder_vbuild-48.AppImage
```

Удобный вариант той же команды (части в порядке следования):

```bash
cat \
  ScreenRecorder_vbuild-48.AppImage.part-aa \
  ScreenRecorder_vbuild-48.AppImage.part-ab \
  ScreenRecorder_vbuild-48.AppImage.part-ac \
  > ScreenRecorder_vbuild-48.AppImage
```

**Проверка контрольной суммы.** Рядом с каждой частью GitHub показывает её
`sha256`. Сверьте хеш собранного файла:

```bash
sha256sum ScreenRecorder_vbuild-48.AppImage
```

Он должен совпасть с указанным в релизе (либо с тем, что выведется при сборке).
Размер собранного файла должен быть равен сумме частей (≈ их общий размер).

> 💡 **Совет:** альтернатива — собрать AppImage локально одним файлом через
> `./build_AppImage.sh`. Такой файл можно скачать целиком, без склейки частей.
> Для обычного (CPU) использования размером <2 ГБ GitHub-релиз отдаёт один файл.

### Требования для сборки AppImage (Linux x86_64)

**Системные пакеты** (проверьте перед сборкой):

| Пакет | Назначение | Обязателен |
|---|---|---|
| `libtcl8.6`, `libtk8.6` | Компиляция `_tkinter` с fontconfig | ✅ да |
| `gcc`, `make` | Компиляция `_tkinter` | ✅ да |
| `curl`, `dpkg` | Скачивание CPython и dev-заголовков | ✅ да |
| `ffmpeg` | Кодирование видео/аудио при запуске | ⚠️ или `bin/linux/ffmpeg` |
| `xdg-desktop-portal`, `pipewire`, `gstreamer1.0-pipewire` | Запись экрана на Wayland | ⚠️ для Wayland |

```bash
sudo apt install libtcl8.6 libtk8.6 gcc make curl ffmpeg \
  xdg-desktop-portal pipewire gstreamer1.0-pipewire
```

**Окружение проекта** — создаётся автоматически скриптом (без sudo):

```bash
bash scripts/setup_env.sh    # CPython 3.14 → .python/, venv, зависимости, _tkinter
```

| Каталог | Что внутри | Размер |
|---|---|---|
| `.python/` | CPython 3.14 (python-build-standalone) | ~113 МБ |
| `.venv/` | Зависимости из `requirements.txt` (torch CPU) | ~2 ГБ |
| `.sdk/` | Dev-заголовки Tcl/Tk/X11 для сборки `_tkinter` | ~60 МБ |
| `tools/` | `appimagetool` для упаковки AppImage | ~10 МБ |

**Порядок сборки с нуля:**

```bash
git clone https://github.com/Freekazoid/recordingScreen
cd recordingScreen
bash scripts/setup_env.sh     # окружение + проверка шрифтов
./build_AppImage.sh           # → output/ScreenRecorder_v<версия>.AppImage
```

> 💡 После **переноса проекта** в другую директорию просто повторно запустите
> `bash scripts/setup_env.sh` — он пересоздаст venv под новый путь.
> Скрипт идемпотентен: готовые шаги пропускаются.

### Переменные окружения для сборки

| Переменная | Описание |
|---|---|
| `APP_VERSION` | Переопределить версию |
| `FFMPEG_LINUX_DIR` | Путь к встроенным ffmpeg бинарникам для Linux |
| `FFMPEG_MACOS_DIR` | Путь к встроенным ffmpeg бинарникам для macOS |
| `FFMPEG_WINDOWS_DIR` | Путь к встроенным ffmpeg бинарникам для Windows |
| `MACOS_TARGET_ARCH` | Архитектура macOS (`arm64`, `x86_64`, `universal2`) |
| `MACOS_SIGN_IDENTITY` | Идентификатор подписи кода macOS |
| `CREATE_DMG=1` | Создать .dmg образ на macOS |
| `CLEANUP_DOCKER_IMAGE` | Удалить Docker образ после сборки Windows |

---

## 🧰 Стек технологий

| Слой | Технология |
|---|---|
| Язык | Python 3.14 (python-build-standalone; CI — 3.11) |
| UI | Tkinter / ttk |
| Захват видео | `mss`, `PIL.ImageGrab`, X11 / ScreenCast-портал (Wayland) |
| Кодирование видео | OpenCV (`cv2.VideoWriter`, XVID) |
| Захват аудио | `ffmpeg` (subprocess), `pw-record` / `pw-link` |
| Склейка A/V | `ffmpeg` (subprocess) |
| Распознавание речи | WhisperX (`faster-whisper large-v3-turbo`), Vosk |
| Диаризация | PyAnnote (`diarize`), NeMo Sortformer (`nemo`) |
| Управление моделями | `huggingface_hub` |
| Системный трей | `pystray` |
| Сборка | PyInstaller (AppImage / .app / .exe) |

---

## 📁 Структура проекта

```
src/
├── main.py                         # Точка входа (+ ensure_portal_identity)
├── gui_window.py                   # Главное окно (Tkinter), логика записи
├── full_screen.py                  # Захват полного экрана
├── area_screen.py                  # Захват области экрана (Wayland/X11)
├── program_screen.py               # Захват окна приложения
├── program_screen_wayland.py       # Запись окна/экрана через портал на Wayland
├── audio_recorder.py               # Запись аудио (PulseAudio/PipeWire/WASAPI)
├── audio_transcriber_service.py    # Транскрипция + диаризация
├── merge_save.py                   # Склейка видео и аудио
├── model_manager.py                # Менеджер загрузки моделей
├── background_tasks.py             # Фоновые процессы (multiprocessing)
├── settings_manager.py             # Единая точка настроек (дефолты из default_settings.json)
├── app_paths.py                    # Пути данных (dev/AppImage)
├── portal_identity.py              # App-id портала для Wayland (cgroup-scope)
├── screencast_frame.py             # Долгоживущая ScreenCast-сессия для снимков области
├── ffmpeg_locator.py               # Поиск ffmpeg в системе
├── tray_icon.py                    # Иконка в системном трее
└── default_settings.json           # Заводские настройки по умолчанию

scripts/
├── setup_env.sh                    # Bootstrap окружения (одной командой)
├── build_tkinter.sh                # Сборка _tkinter против Tcl/Tk 8.6 (fontconfig)
├── fetch_tk_dev_headers.sh         # Dev-заголовки из deb-пакетов (без sudo)
├── test_core_modules.py            # Быстрые тесты ядра (без GUI/D-Bus)
├── test_settings.py                # Тесты подсистемы настроек
└── test_fresh_install.py           # Регресс-тест «свежей установки» (AppImage)

build_AppImage.sh                   # Сборка Linux AppImage
```

---

## 📌 Примечания

- Приложение разработано в первую очередь для **Linux**; поддержка Windows — частичная
- Для работы транскрипции и диаризации требуются модели машинного обучения (скачиваются из интерфейса)
- Для доступа к моделям HF-токен нужен только **PyAnnote**; WhisperX, NeMo Sortformer и Vosk скачиваются без него
- Весь интерфейс — на **русском языке**
- Для записи окон в **Wayland** необходимы системные пакеты: `python3-gi`, `xdg-desktop-portal`, `pipewire`, `gstreamer1.0-pipewire`, а также набор плагинов `gstreamer1.0-plugins-good` / `gstreamer1.0-plugins-bad`

---

## ⚙️ Настройки (settings.json)

Заводские значения хранятся в `src/default_settings.json` и поставляются
внутри приложения. Пользовательский `settings.json` создаётся автоматически
при первом запуске — его можно править вручную.

> 📦 **В AppImage** маунт read-only, поэтому данные хранятся в
> `~/.local/share/ScreenRecorder/`: `settings.json`, модели, `.hf_token`.
> В dev-режиме конфиг лежит в корне проекта. Пути к моделям и результатам
> переопределяются в настройках приложения.
>
> 🔄 Устаревшие значения переносятся автоматически (`whisper` → `whisperx`,
> `sherpa` → `nemo`, длинные подписи качества/звука — в короткие).

| Ключ | Описание | Где используется |
|---|---|---|
| `transcription_engine` | Движок распознавания: `whisperx` или `vosk` | Транскрипция и постобработка |
| `compute_device` | Устройство для распознавания: `auto`, `gpu`, `cpu` | Транскрибатор |
| `diarization_method` | Метод спикеров: `none`, `diarize`, `nemo` | Диаризация и постобработка |
| `enable_transcription` | Включить распознавание текста | Постобработка, ручная транскрипция |
| `enable_diarization` | Включить разметку спикеров | Постобработка, ручная транскрипция |
| `debug_mode` | Сохранять промежуточные файлы | Постобработка (очистка временных файлов) |
| `expected_speakers` | Ожидаемое число спикеров | Диаризация/кластеризация |
| `min_speakers` | Минимум спикеров | Диаризация/кластеризация |
| `max_speakers` | Максимум спикеров | Диаризация/кластеризация |
| `output_format` | Формат финального видео: `mp4`, `mkv`, `webm`, `avi`, `mov`, `flv` | Экспорт видео |
| `video_quality` | Качество/CRF (строка из UI) | Экспорт видео |
| `audio_track_mode` | Режим аудио: оригинал/сжатие/удалить | Экспорт видео |
| `models_dir` | Директория хранения моделей | Загрузка/использование моделей |
| `output_dir` | Директория для результатов | Финальные видео/тексты |

Примечание: если редактируете `settings.json` вручную, перезапустите приложение.
