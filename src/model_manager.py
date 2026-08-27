import os
import shutil
import subprocess
import sys
import threading
import time

_CREATIONFLAGS_NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0

from huggingface_hub import login, snapshot_download

from app_paths import get_writable_base_dir

APP_BASE_DIR = get_writable_base_dir()

_MODEL_DIR = os.path.join(APP_BASE_DIR, "model")
HF_TOKEN_FILE = os.path.join(APP_BASE_DIR, ".hf_token")

VOSK_MODEL_LOCAL = "vosk-model-ru-0.42.zip"
VOSK_MODEL_URL = "https://alphacephei.com/vosk/models/vosk-model-ru-0.42.zip"
VOSK_EXTRACTED_DIR = "vosk-model-ru-0.42"
VOSK_REQUIRED_ITEMS = ["am", "conf", "graph", "ivector"]

def _build_model_specs(model_dir):
    return {
        "whisper": {
            "repo_id": "openai/whisper-medium",
            "local_dir": os.path.join(model_dir, "whisper-medium"),
            "requires_token": True,
            # Только нужные файлы: без дубликатов весов (pytorch_model.bin,
            # tf_model.h5, flax_model.msgpack) репозиторий ~12 ГБ -> ~1.5 ГБ.
            "download_patterns": [
                "config.json",
                "generation_config.json",
                "preprocessor_config.json",
                "tokenizer.json",
                "tokenizer_config.json",
                "vocab.json",
                "merges.txt",
                "special_tokens_map.json",
                "added_tokens.json",
                "normalizer.json",
                "model.safetensors",
            ],
            "display": "Whisper"
        },
        "whisperx": {
            "repo_id": "mobiuslabsgmbh/faster-whisper-large-v3-turbo",
            "local_dir": os.path.join(model_dir, "faster-whisper-large-v3-turbo"),
            "requires_token": False,
            # CTranslate2-формат: без дубликатов весов, ~1.6 ГБ.
            "download_patterns": [
                "config.json",
                "model.bin",
                "preprocessor_config.json",
                "tokenizer.json",
                "vocabulary.json",
            ],
            "display": "WhisperX"
        },
        "pyannote": {
            "repo_id": "pyannote/speaker-diarization-3.1",
            "local_dir": os.path.join(model_dir, "pyannote_speaker-diarization-3.1"),
            "requires_token": True,
            "download_patterns": [
                "config.yaml",
            ],
            "display": "PyAnnote"
        },
        "nemo": {
            "repo_id": "nvidia/diar_sortformer_4spk-v1",
            "local_dir": os.path.join(model_dir, "diar_sortformer_4spk-v1"),
            "requires_token": False,
            "download_patterns": [
                "diar_sortformer_4spk-v1.nemo",
            ],
            "display": "NeMo"
        }
    }

MODEL_SPECS = _build_model_specs(_MODEL_DIR)

def set_model_dir(model_dir):
    global _MODEL_DIR, MODEL_SPECS
    _MODEL_DIR = model_dir
    MODEL_SPECS = _build_model_specs(_MODEL_DIR)

MODEL_DIR_NAMES = {
    "whisper": "whisper-medium",
    "whisperx": "faster-whisper-large-v3-turbo",
    "pyannote": "pyannote_speaker-diarization-3.1",
    "nemo": "diar_sortformer_4spk-v1",
}


def find_existing_models_root(models_dir_setting):
    """Ищет уже скачанные модели внутри указанной пользователем папки.

    Поддерживаемые раскладки:
      <папка>/models/<модель>  — каноничная раскладка приложения
      <папка>/model/<модель>   — раскладка по умолчанию (dev/XDG)
      <папка>/<модель>         — пользователь указал прямо на модели
    Возвращает найденный корень или None.
    """
    base = os.path.abspath(models_dir_setting)
    best_root = None
    best_score = 0
    for cand in (os.path.join(base, "models"), base, os.path.join(base, "model")):
        score = 0
        for name in set(MODEL_DIR_NAMES.values()):
            if os.path.isdir(os.path.join(cand, name)):
                score += 1
        if os.path.isfile(os.path.join(cand, VOSK_MODEL_LOCAL)) or os.path.isdir(
            os.path.join(cand, VOSK_EXTRACTED_DIR)
        ):
            score += 1
        if score > best_score:
            best_root, best_score = cand, score
    return best_root


def resolve_models_path(models_dir_setting):
    """Настройка «Папка моделей» -> фактический корень моделей.

    Если модели уже есть в альтернативной раскладке, используем её,
    чтобы не скачивать заново; иначе каноничная <папка>/models.
    """
    found = find_existing_models_root(models_dir_setting)
    return found or os.path.join(os.path.abspath(models_dir_setting), "models")



def get_model_dir():
    return _MODEL_DIR

MODEL_REQUIRED_FILES = {
    "whisper": [
        "config.json",
        "preprocessor_config.json",
        "tokenizer.json",
    ],
    "whisperx": [
        "model.bin",
        "config.json",
        "tokenizer.json",
    ],
    "pyannote": [
        "config.yaml",
    ],
    "nemo": [
        "diar_sortformer_4spk-v1.nemo",
    ],
}

def is_whisper_downloaded():
    return is_model_downloaded("whisper")

def is_vosk_downloaded():
    candidates = [
        os.path.join(get_model_dir(), VOSK_MODEL_LOCAL),
        os.path.join(get_model_dir(), VOSK_EXTRACTED_DIR),
        os.path.join(get_model_dir(), "vosk_model", VOSK_EXTRACTED_DIR),
    ]
    for vosk_path in candidates:
        if not os.path.isdir(vosk_path):
            continue
        if all(os.path.exists(os.path.join(vosk_path, item)) for item in VOSK_REQUIRED_ITEMS):
            return True
    return False

def is_pyannote_downloaded():
    return is_model_downloaded("pyannote")

def delete_model(model_name):
    """Удаляет локальные файлы модели. Возвращает True, если удалили."""
    import shutil

    removed = False
    if model_name == "vosk":
        candidates = [
            os.path.join(get_model_dir(), VOSK_MODEL_LOCAL),
            os.path.join(get_model_dir(), VOSK_EXTRACTED_DIR),
            os.path.join(get_model_dir(), "vosk_model"),
        ]
        for path in candidates:
            if os.path.isdir(path):
                shutil.rmtree(path, ignore_errors=True)
                removed = True
            elif os.path.isfile(path):
                try:
                    os.remove(path)
                    removed = True
                except OSError:
                    pass
        return removed

    spec = MODEL_SPECS.get(model_name)
    if spec is None:
        return False
    local_dir = spec["local_dir"]
    if os.path.isdir(local_dir):
        shutil.rmtree(local_dir, ignore_errors=True)
        removed = True
    return removed

def is_nemo_downloaded():
    return is_model_downloaded("nemo")

def get_model_status():
    return {
        "whisper": is_whisper_downloaded(),
        "whisperx": is_model_downloaded("whisperx"),
        "vosk": is_vosk_downloaded(),
        "pyannote": is_pyannote_downloaded(),
        "nemo": is_nemo_downloaded()
    }

def download_model(model_name, token, log_func, progress_func=None):
    """Download specific model."""
    if model_name == "vosk":
        return download_vosk_model(log_func, progress_func)

    spec = MODEL_SPECS.get(model_name)
    if spec is None:
        log_func(f"Неизвестная модель: {model_name}")
        return False

    if is_model_downloaded(model_name):
        log_func(f"{spec['display']} уже скачан")
        return True

    required_token = token or load_hf_token() if spec.get("requires_token") else None
    if spec.get("requires_token") and not required_token:
        log_func("Нужен HF_TOKEN")
        return False

    log_func(f"Скачиваем {spec['display']}...")
    os.makedirs(spec["local_dir"], exist_ok=True)
    try:
        if spec.get("requires_token"):
            login(token=required_token)
        snapshot_download(
            repo_id=spec["repo_id"],
            local_dir=spec["local_dir"],
            local_dir_use_symlinks=False,
            resume_download=True,
            allow_patterns=spec.get("download_patterns"),
            token=required_token
        )
        log_func(f"{spec['display']} готов")
        return True
    except Exception as e:
        log_func(f"Ошибка {spec['display']}: {e}")
        return False

VOSK_HF_MIRROR_REPO = "Derur/vosk-models"
VOSK_HF_MIRROR_PATTERN = f"stt/ru/{VOSK_EXTRACTED_DIR}/*"


def _download_vosk_from_hf(extract_tmp: str, log_func, progress_func=None) -> bool:
    """Скачивает vosk-model-ru-0.42 из HF-зеркала в extract_tmp.

    Возвращает True при успехе. Ошибки логируются, но не бросаются —
    вызывающий код откатится к официальному сайту.
    """
    try:
        from huggingface_hub import snapshot_download
        from tqdm import tqdm
    except Exception as e:
        log_func(f"HF-зеркало недоступно (нет библиотек): {e}")
        return False

    class _ProgressTqdm(tqdm):
        def update(self, n=1):
            super().update(n)
            if progress_func and self.total:
                try:
                    progress_func(min(99.0, self.n * 100.0 / self.total))
                except Exception:
                    pass

    os.makedirs(extract_tmp, exist_ok=True)
    log_func("Скачиваем из зеркала HuggingFace...")
    try:
        snapshot_download(
            repo_id=VOSK_HF_MIRROR_REPO,
            allow_patterns=[VOSK_HF_MIRROR_PATTERN],
            local_dir=extract_tmp,
            tqdm_class=_ProgressTqdm,
        )
        return True
    except Exception as e:
        log_func(f"Ошибка зеркала HuggingFace: {e}")
        return False


def download_vosk_model(log_func, progress_func=None):
    """Download Vosk model from official website."""
    import urllib.request
    import zipfile

    if is_vosk_downloaded():
        log_func("Vosk уже скачан")
        return True

    vosk_path = os.path.join(get_model_dir(), VOSK_MODEL_LOCAL)
    log_func("Скачиваем Vosk...")
    os.makedirs(get_model_dir(), exist_ok=True)

    temp_zip = os.path.join(get_model_dir(), "vosk_model.zip")
    temp_part = temp_zip + ".part"
    extract_tmp = os.path.join(get_model_dir(), "_vosk_extract_tmp")

    try:
        if os.path.exists(temp_part):
            os.remove(temp_part)
        if os.path.exists(temp_zip):
            os.remove(temp_zip)
        if os.path.isdir(extract_tmp):
            shutil.rmtree(extract_tmp, ignore_errors=True)

        with open(temp_part, "wb"):
            pass

        # Основной путь: официальная модель 0.42, зеркалированная на HuggingFace.
        # alphacephei.com часто отдаёт файл со скоростью <1 КБ/с и не годится.
        if _download_vosk_from_hf(extract_tmp, log_func, progress_func):
            extracted_folder = os.path.join(
                extract_tmp, "stt", "ru", VOSK_EXTRACTED_DIR
            )
            if not os.path.isdir(extracted_folder):
                raise RuntimeError("HF-зеркало вернуло неполную структуру модели Vosk")

            final_dir = os.path.join(get_model_dir(), VOSK_EXTRACTED_DIR)
            if os.path.isdir(final_dir):
                shutil.rmtree(final_dir, ignore_errors=True)
            shutil.move(extracted_folder, final_dir)
            shutil.rmtree(extract_tmp, ignore_errors=True)

            if progress_func:
                progress_func(100.0)
            log_func("Vosk готов")
            return True

        log_func("HF-зеркало недоступно, пробую официальный сайт...")

        downloaded_with_curl = False
        curl_bin = shutil.which("curl")
        if curl_bin:
            cmd = [
                curl_bin,
                "-L",
                "--fail",
                "--retry", "3",
                "--connect-timeout", "20",
                "--speed-limit", "10240",
                "--speed-time", "30",
                "--output", temp_part,
                VOSK_MODEL_URL,
            ]
            kwargs = {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}
            if _CREATIONFLAGS_NO_WINDOW:
                kwargs["creationflags"] = _CREATIONFLAGS_NO_WINDOW
            proc = subprocess.Popen(cmd, **kwargs)
            last_size = 0
            last_change = time.time()
            while proc.poll() is None:
                time.sleep(1)
                size = os.path.getsize(temp_part) if os.path.exists(temp_part) else 0
                if size != last_size:
                    last_size = size
                    last_change = time.time()
                if time.time() - last_change > 60:
                    proc.terminate()
                    break
            if proc.poll() == 0 and os.path.getsize(temp_part) > 0:
                downloaded_with_curl = True

        if not downloaded_with_curl:
            if curl_bin:
                log_func("Скачивание через curl не удалось, пробую резервный метод")
            req = urllib.request.Request(VOSK_MODEL_URL, headers={"User-Agent": "Mozilla/5.0"})
            opener = urllib.request.build_opener(urllib.request.ProxyHandler())
            with opener.open(req, timeout=120) as response, open(temp_part, "ab") as out:
                total = response.headers.get("Content-Length")
                total_size = int(total) if total and total.isdigit() else 0
                downloaded = 0
                chunk_size = 1024 * 1024
                while True:
                    chunk = response.read(chunk_size)
                    if not chunk:
                        break
                    out.write(chunk)
                    downloaded += len(chunk)
                    if progress_func and total_size > 0:
                        percent = min(100, int(downloaded * 100 / total_size))
                        progress_func(percent)

        os.replace(temp_part, temp_zip)
        log_func("Распаковываем...")

        os.makedirs(extract_tmp, exist_ok=True)
        with zipfile.ZipFile(temp_zip, "r") as z:
            z.extractall(extract_tmp)

        extracted_folder = None
        direct_children = [
            os.path.join(extract_tmp, name)
            for name in os.listdir(extract_tmp)
            if os.path.isdir(os.path.join(extract_tmp, name))
        ]

        for candidate in direct_children:
            required_items = ["am", "conf", "graph", "ivector"]
            if all(os.path.exists(os.path.join(candidate, item)) for item in required_items):
                extracted_folder = candidate
                break

        if extracted_folder is None:
            named_candidates = [c for c in direct_children if os.path.basename(c).startswith("vosk-model")]
            if named_candidates:
                extracted_folder = named_candidates[0]

        if extracted_folder is None:
            raise RuntimeError("Не удалось найти распакованную папку модели Vosk")

        if os.path.exists(vosk_path):
            shutil.rmtree(vosk_path)
        shutil.move(extracted_folder, vosk_path)

        log_func("Vosk готов")
        return True
    except Exception as e:
        log_func(f"Ошибка Vosk: {e}")
        return False
    finally:
        if os.path.exists(temp_part):
            try:
                os.remove(temp_part)
            except Exception:
                pass
        if os.path.exists(temp_zip):
            try:
                os.remove(temp_zip)
            except Exception:
                pass
        if os.path.isdir(extract_tmp):
            shutil.rmtree(extract_tmp, ignore_errors=True)

def _try_keyring(service="recordingScreen", key="hf_token"):
    try:
        import keyring
        return keyring.get_password(service, key)
    except Exception:
        return None

def _save_keyring(token, service="recordingScreen", key="hf_token"):
    try:
        import keyring
        keyring.set_password(service, key, token.strip())
        return True
    except Exception:
        return False

def save_hf_token(token):
    if _save_keyring(token):
        return
    os.makedirs(os.path.dirname(HF_TOKEN_FILE) or ".", exist_ok=True)
    with open(HF_TOKEN_FILE, "w", encoding="utf-8") as f:
        f.write(token.strip())
    try:
        os.chmod(HF_TOKEN_FILE, 0o600)
    except Exception:
        pass

def load_hf_token():
    stored = _try_keyring()
    if stored:
        return stored
    if os.path.isfile(HF_TOKEN_FILE):
        with open(HF_TOKEN_FILE, encoding="utf-8") as f:
            return f.read().strip()
    return None

def is_model_downloaded(model_key):
    if model_key == "vosk":
        return is_vosk_downloaded()

    spec = MODEL_SPECS.get(model_key)
    if spec is None:
        # support legacy repo ids
        for candidate in MODEL_SPECS.values():
            if candidate["repo_id"] == model_key:
                spec = candidate
                break
    if spec is None:
        return False
    path = spec["local_dir"]
    if not os.path.isdir(path):
        return False

    required_files = MODEL_REQUIRED_FILES.get(model_key, [])
    for rel_name in required_files:
        if not os.path.exists(os.path.join(path, rel_name)):
            return False

    with os.scandir(path) as entries:
        return any(True for _ in entries)

def check_and_download_models_async(
    root, log, set_model_status, ask_hf_token_gui,
    model_status_canvas, oval, model_status_text, model_status_color,
    recording_status_text, downloading_flag
):
    def worker():
        set_model_status("downloading")
        downloading_flag["running"] = True
        os.makedirs(get_model_dir(), exist_ok=True)
        token = os.environ.get("HF_TOKEN") or load_hf_token()
        if not token:
            token = None
            def get_token_from_user():
                nonlocal token
                token = ask_hf_token_gui(root)
            root.after(0, get_token_from_user)
            import time
            while token is None:
                time.sleep(0.1)
            if token:
                save_hf_token(token)
        def ui_log(message):
            root.after(0, lambda m=message: log(m))

        for key in MODEL_SPECS.keys():
            download_model(key, token, ui_log)

        # Download Vosk model
        if not is_vosk_downloaded():
            root.after(0, lambda: log("Скачиваем Vosk..."))
            try:
                download_vosk_model(lambda msg: root.after(0, lambda m=msg: log(m)))
            except Exception as e:
                root.after(0, lambda e=e: log(f"❌ Ошибка Vosk: {e}"))
        else:
            root.after(0, lambda: log("Vosk модель уже есть."))

        downloading_flag["running"] = False
        # Check final status
        hf_ready = all(is_model_downloaded(key) for key in MODEL_SPECS)
        vosk_ready = is_vosk_downloaded()

        if hf_ready and vosk_ready:
            root.after(0, lambda: log("Все модели готовы."))
            root.after(0, lambda: set_model_status("ready"))
        elif hf_ready and not vosk_ready:
            root.after(0, lambda: log("HF модели готовы. Vosk нужно скачать вручную."))
            root.after(0, lambda: set_model_status("ready"))
        else:
            root.after(0, lambda: set_model_status("not_ready"))

    # Check initial status
    hf_ready = all(is_model_downloaded(key) for key in MODEL_SPECS)
    vosk_ready = is_vosk_downloaded()

    if hf_ready:
        set_model_status("ready")
        if not vosk_ready:
            log("HF модели готовы. Для Vosk скачайте модель вручную.")
        else:
            log("Все модели готовы.")
    else:
        set_model_status("downloading")
        threading.Thread(target=worker, daemon=True).start()
