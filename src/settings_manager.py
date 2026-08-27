"""Единая точка работы с настройками приложения.

Источник заводских дефолтов — файл default_settings.json рядом с модулем
(в AppImage — внутри _internal/, путь через sys._MEIPASS). Любое добавление
или удаление настройки делается ТОЛЬКО здесь:
  1. Добавить ключ в src/default_settings.json (значение = тип-образец: str/bool).
  2. При необходимости — миграцию старых значений в MIGRATIONS.
Пользовательский конфиг хранится отдельно в каталоге данных
(get_writable_base_dir()/settings.json); при первом запуске он создаётся
с заводскими значениями и доступен для ручного редактирования.
Остальной код читает и пишет настройки только через
load_settings()/save_settings().
"""

import json
import os
import sys

from app_paths import get_writable_base_dir


DEFAULT_SETTINGS_FILE = "default_settings.json"


def default_settings_file() -> str:
    """Путь к заводским дефолтам (в комплекте приложения)."""
    if getattr(sys, "frozen", False):
        base = getattr(sys, "_MEIPASS", os.path.dirname(sys.executable))
        return os.path.join(base, DEFAULT_SETTINGS_FILE)
    return os.path.join(os.path.dirname(os.path.abspath(__file__)), DEFAULT_SETTINGS_FILE)


def _load_defaults() -> dict:
    """Чтение заводских дефолтов из JSON-файла; {} при сбое (с диагностикой)."""
    path = default_settings_file()
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict):
            raise ValueError("файл дефолтов не является словарём")
        # Значения-образцы типов допускают только str/bool
        for key, value in data.items():
            if not isinstance(value, (str, bool)):
                raise ValueError(f"недопустимый тип значения {key!r}: {type(value).__name__}")
        return data
    except Exception as e:
        print(f"Ошибка чтения заводских настроек {path}: {e}")
        try:
            from logging_utils import write_error_report

            write_error_report("settings", f"Не удалось прочитать заводские настройки: {e}", extra=str(path))
        except Exception:
            pass
        return {}


DEFAULTS: dict = _load_defaults()

# Перенос legacy-значений при загрузке: ключ -> {старое_значение: новое}
MIGRATIONS: dict = {
    "transcription_engine": {"whisper": "whisperx"},
    "diarization_method": {"sherpa": "nemo"},
    # Сокращение подписей селектов (v2026.08): старый длинный текст -> новый
    "video_quality": {
        "Отличное (CRF 18) - почти без потерь": "Отличное (CRF 18)",
        "Хорошее (CRF 23) - по умолчанию": "Хорошее (CRF 23)",
        "Среднее (CRF 28) - заметная разница": "Среднее (CRF 28)",
        "Сильное сжатие (CRF 35) - маленький файл": "Сильное сжатие (CRF 35)",
    },
    "audio_track_mode": {
        "Оставить как есть (без пережатия)": "Как есть (без пережатия)",
        "Сжать звук (AAC 128k)": "Сжать (AAC 128k)",
        "Удалить звук (только видео)": "Без звука",
    },
}

# Строковые представления булевых значений (для ручного редактирования конфига)
_BOOL_TRUE = {"1", "true", "yes", "on", "да", "истина", "вкл"}
_BOOL_FALSE = {"", "0", "false", "no", "off", "нет", "ложь", "выкл"}


def _coerce_bool(value) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        v = value.strip().lower()
        if v in _BOOL_TRUE:
            return True
        if v in _BOOL_FALSE:
            return False
    return bool(value)


def config_file() -> str:
    return os.path.join(get_writable_base_dir(), "settings.json")


def normalize(settings: dict) -> dict:
    """Дефолты + миграции + приведение типов по образцу DEFAULTS."""
    result = dict(DEFAULTS)
    if isinstance(settings, dict):
        result.update({k: v for k, v in settings.items() if k in DEFAULTS})
    for key, mapping in MIGRATIONS.items():
        if result.get(key) in mapping:
            result[key] = mapping[result[key]]
    for key, sample in DEFAULTS.items():
        value = result[key]
        if isinstance(sample, bool):
            result[key] = _coerce_bool(value)
        elif isinstance(sample, str):
            result[key] = str(value) if value is not None else ""
    return result


def load_settings() -> dict:
    """Загрузка настроек с дефолтами и миграциями.

    Если пользовательского файла ещё нет — он создаётся с заводскими
    значениями, чтобы его можно было править вручную.
    """
    path = config_file()
    loaded = {}
    file_exists = os.path.exists(path)
    if file_exists:
        try:
            with open(path, encoding="utf-8") as f:
                loaded = json.load(f)
            if not isinstance(loaded, dict):
                raise ValueError("settings.json не является словарём")
        except Exception as e:
            print(f"Ошибка загрузки {path}: {e}. Используются настройки по умолчанию")
            loaded = {}
    normalized = normalize(loaded)
    if not file_exists:
        save_settings(normalized)
    return normalized


def save_settings(settings: dict):
    """Атомарное сохранение только известных ключей."""
    path = config_file()
    data = normalize(settings)
    tmp_path = path + ".tmp"
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(tmp_path, "w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        os.replace(tmp_path, path)
    except Exception as e:
        print(f"Ошибка сохранения настроек: {e}")
        try:
            from logging_utils import write_error_report

            write_error_report("settings", f"Не удалось сохранить настройки: {e}", extra=str(path))
        except Exception:
            pass


__all__ = [
    "DEFAULTS",
    "DEFAULT_SETTINGS_FILE",
    "MIGRATIONS",
    "config_file",
    "default_settings_file",
    "load_settings",
    "save_settings",
    "normalize",
]
