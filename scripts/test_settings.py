"""Тесты подсистемы настроек приложения (settings_manager + app_paths).

Запуск:  .venv/bin/python scripts/test_settings.py

Проверяет:
  * файл заводских дефолтов default_settings.json (валидность, совпадение с DEFAULTS);
  * пути хранения конфига в dev и frozen-режимах;
  * normalize(): дефолты, фильтрация неизвестных ключей, приведение типов, миграции;
  * load_settings()/save_settings(): материализация при первом запуске,
    устойчивость к битому JSON, атомарность записи, миграции при записи.
Не требует дисплея, D-Bus или PipeWire.
"""
from __future__ import annotations

import json
import os
import stat
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
sys.path.insert(0, SRC)

failures: list[str] = []


def check(name: str, condition: bool, detail: str = ""):
    status = "OK" if condition else "FAIL"
    print(f"[{status}] {name}" + (f" -> {detail}" if detail and not condition else ""))
    if not condition:
        failures.append(name)


import app_paths
import settings_manager as sm


def patch_env(**kwargs):
    """Контекстный менеджер для подмены sys.frozen/_MEIPASS/executable/XDG_DATA_HOME."""
    import contextlib

    @contextlib.contextmanager
    def _ctx():
        saved = {
            "frozen": getattr(sys, "frozen", None),
            "meipass": getattr(sys, "_MEIPASS", None),
            "executable": sys.executable,
            "xdg": os.environ.get("XDG_DATA_HOME"),
        }
        had_frozen = "frozen" in dir(sys) or saved["frozen"] is not None
        try:
            for k, v in kwargs.items():
                if k == "xdg":
                    if v is None:
                        os.environ.pop("XDG_DATA_HOME", None)
                    else:
                        os.environ["XDG_DATA_HOME"] = v
                elif k == "frozen":
                    sys.frozen = v  # type: ignore[attr-defined]
                elif k == "meipass":
                    if v is None:
                        if hasattr(sys, "_MEIPASS"):
                            del sys._MEIPASS  # type: ignore[attr-defined]
                    else:
                        sys._MEIPASS = v  # type: ignore[attr-defined]
                elif k == "executable":
                    sys.executable = v
            yield
        finally:
            # восстановление
            if saved["frozen"] is None and "frozen" in vars(sys):
                del sys.frozen  # type: ignore[attr-defined]
            elif saved["frozen"] is not None:
                sys.frozen = saved["frozen"]  # type: ignore[attr-defined]
            if saved["meipass"] is None:
                if hasattr(sys, "_MEIPASS"):
                    del sys._MEIPASS  # type: ignore[attr-defined]
            else:
                sys._MEIPASS = saved["meipass"]  # type: ignore[attr-defined]
            sys.executable = saved["executable"]
            if saved["xdg"] is None:
                os.environ.pop("XDG_DATA_HOME", None)
            else:
                os.environ["XDG_DATA_HOME"] = saved["xdg"]

    return _ctx()


# ── A. Файл заводских дефолтов ──────────────────────────────────────────
def test_defaults_file():
    path = sm.default_settings_file()
    check("A1 файл дефолтов существует", os.path.isfile(path), path)

    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        ok_parse = True
    except Exception as e:
        data, ok_parse = {}, False
        check("A2 файл дефолтов — валидный JSON", False, str(e))
    if ok_parse:
        check("A2 файл дефолтов — валидный JSON", True)

    check("A3 файл дефолтов — словарь", isinstance(data, dict))
    bad_types = [k for k, v in data.items() if not isinstance(v, (str, bool))]
    check("A4 все значения — str или bool", not bad_types, str(bad_types))
    bad_keys = [k for k in data if not isinstance(k, str) or not k.strip()]
    check("A5 ключи — непустые строки", not bad_keys, str(bad_keys))

    check("A6 DEFAULTS совпадает с файлом", sm.DEFAULTS == data)

    # Критичный контракт приложения: эти ключи обязаны существовать
    required = (
        "transcription_engine",
        "diarization_method",
        "enable_transcription",
        "enable_diarization",
        "debug_mode",
        "output_format",
    )
    missing = [k for k in required if k not in sm.DEFAULTS]
    check("A7 критичные ключи присутствуют", not missing, str(missing))


# ── B. Пути хранения ────────────────────────────────────────────────────
def test_paths():
    # B1: config_file() строится от writable base dir
    with tempfile.TemporaryDirectory() as td:
        old = sm.get_writable_base_dir
        try:
            sm.get_writable_base_dir = lambda: td  # type: ignore[assignment]
            check("B1 config_file = <base>/settings.json",
                  sm.config_file() == os.path.join(td, "settings.json"), sm.config_file())
        finally:
            sm.get_writable_base_dir = old  # type: ignore[assignment]

    # B2: dev-режим -> корень проекта
    dev_base = app_paths.get_writable_base_dir()
    check("B2 dev: база = корень проекта",
          dev_base == os.path.dirname(SRC), dev_base)

    # B3: frozen + каталог бинарника доступен для записи -> рядом с бинарником
    with tempfile.TemporaryDirectory() as td:
        exe = os.path.join(td, "ScreenRecorder")
        open(exe, "w").close()
        with patch_env(frozen=True, executable=exe):
            check("B3 frozen+записываемый каталог: база рядом с бинарником",
                  app_paths.get_writable_base_dir() == td,
                  app_paths.get_writable_base_dir())

    # B4: frozen + read-only каталог (маунт AppImage) -> XDG_DATA_HOME/ScreenRecorder
    with tempfile.TemporaryDirectory() as td:
        ro_bin = os.path.join(td, "bin")
        os.makedirs(ro_bin)
        exe = os.path.join(ro_bin, "ScreenRecorder")
        open(exe, "w").close()
        os.chmod(ro_bin, stat.S_IRUSR | stat.S_IXUSR | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH)
        xdg = os.path.join(td, "xdg", "nested")
        try:
            with patch_env(frozen=True, executable=exe, xdg=xdg):
                got = app_paths.get_writable_base_dir()
                check("B4 frozen+read-only: база в XDG_DATA_HOME",
                      got == os.path.join(xdg, "ScreenRecorder"), got)
        finally:
            os.chmod(ro_bin, 0o755)

    # B5: frozen без _MEIPASS -> дефолты ищутся рядом с бинарником (fallback)
    with tempfile.TemporaryDirectory() as td:
        fake_bin = os.path.join(td, "usr", "bin")
        os.makedirs(fake_bin)
        shutil_copy = __import__("shutil").copy
        shutil_copy(sm.default_settings_file(), os.path.join(fake_bin, "default_settings.json"))
        exe = os.path.join(fake_bin, "ScreenRecorder")
        open(exe, "w").close()
        with patch_env(frozen=True, executable=exe, meipass=None):
            check("B5 frozen fallback: default_settings_file рядом с бинарником",
                  sm.default_settings_file() == os.path.join(fake_bin, "default_settings.json"),
                  sm.default_settings_file())


# ── C. normalize(): типы, фильтры, миграции ─────────────────────────────
def test_normalize():
    n = sm.normalize({})
    check("C1 normalize({}) = полные дефолты", n == sm.DEFAULTS)
    check("C2 normalize({}) не отдаёт ссылку на DEFAULTS", n is not sm.DEFAULTS)

    n = sm.normalize({"unknown_key": 123})
    check("C3 неизвестные ключи отбрасываются", "unknown_key" not in n)

    n = sm.normalize({"expected_speakers": None})
    check("C4 None -> '' для str-ключей", n["expected_speakers"] == "")

    n = sm.normalize({"expected_speakers": 5})
    check("C5 int приводится к строке", n["expected_speakers"] == "5")

    n = sm.normalize({"debug_mode": True})
    check("C6 bool проходит без изменений (identity)", n["debug_mode"] is True)

    # Строки в bool-настройках: осмысленные true/false, а не bool("false")==True
    for raw, expected in (("false", False), ("0", False), ("no", False),
                          ("", False), ("true", True), ("1", True)):
        n = sm.normalize({"debug_mode": raw})
        check(f"C7 bool из строки {raw!r} -> {expected}", n["debug_mode"] is expected,
              repr(n["debug_mode"]))

    n = sm.normalize({"transcription_engine": "whisper"})
    check("C8 миграция whisper->whisperx", n["transcription_engine"] == "whisperx")
    n = sm.normalize({"diarization_method": "sherpa"})
    check("C9 миграция sherpa->nemo", n["diarization_method"] == "nemo")
    n = sm.normalize({"diarization_method": "diarize"})
    check("C10 не-legacy значения не трогаются", n["diarization_method"] == "diarize")

    # Сокращение подписей селектов: длинный текст -> короткий
    n = sm.normalize({"video_quality": "Хорошее (CRF 23) - по умолчанию"})
    check("C11 миграция video_quality (длинное->короткое)",
          n["video_quality"] == "Хорошее (CRF 23)")
    n = sm.normalize({"audio_track_mode": "Оставить как есть (без пережатия)"})
    check("C12 миграция audio_track_mode (длинное->короткое)",
          n["audio_track_mode"] == "Как есть (без пережатия)")

    # Короткие подписи из GUI-словарей проходят без изменений
    n = sm.normalize({"video_quality": "Сильное сжатие (CRF 35)",
                      "audio_track_mode": "Без звука"})
    check("C13 новые подписи сохраняются как есть",
          n["video_quality"] == "Сильное сжатие (CRF 35)"
          and n["audio_track_mode"] == "Без звука")


# ── D. load_settings / save_settings ────────────────────────────────────
def test_load_save():
    with tempfile.TemporaryDirectory() as td:
        old = sm.get_writable_base_dir
        try:
            sm.get_writable_base_dir = lambda: td  # type: ignore[assignment]
            cfg = os.path.join(td, "settings.json")

            # D1: первый запуск — конфиг материализуется с заводскими значениями
            loaded = sm.load_settings()
            check("D1 первый запуск создаёт конфиг", os.path.isfile(cfg))
            with open(cfg, encoding="utf-8") as f:
                on_disk = json.load(f)
            check("D2 на диске — полные заводские значения", on_disk == loaded == sm.DEFAULTS)

            # D3: повторная загрузка читает тот же файл
            again = sm.load_settings()
            check("D3 повторный load стабилен", again == loaded)

            # D4/D5: битый JSON -> дефолты, файл пользователя НЕ перезаписывается
            broken = "{ это не json"
            with open(cfg, "w", encoding="utf-8") as f:
                f.write(broken)
            loaded = sm.load_settings()
            check("D4 битый JSON -> заводские дефолты без падения", loaded == sm.DEFAULTS)
            with open(cfg, encoding="utf-8") as f:
                check("D5 битый файл не затирается", f.read() == broken)

            # D6: JSON-корень не словарь -> дефолты
            with open(cfg, "w", encoding="utf-8") as f:
                json.dump(["массив"], f)
            loaded = sm.load_settings()
            check("D6 корень-массив -> дефолты", loaded == sm.DEFAULTS)

            # D7/D8: сохранение атомарно (без .tmp-хвостов) и фильтрует ключи
            sm.save_settings({"debug_mode": True, "hack_key": "x"})
            leftovers = [fn for fn in os.listdir(td) if fn.endswith(".tmp")]
            check("D7 нет .tmp после успешной записи", not leftovers, str(leftovers))
            with open(cfg, encoding="utf-8") as f:
                on_disk = json.load(f)
            check("D8 неизвестные ключи не попадают на диск", "hack_key" not in on_disk)
            check("D9 значение сохранилось", on_disk["debug_mode"] is True)

            # D10: миграция применяется при записи
            sm.save_settings({"transcription_engine": "whisper"})
            with open(cfg, encoding="utf-8") as f:
                check("D10 миграция при записи (whisper->whisperx)",
                      json.load(f)["transcription_engine"] == "whisperx")

            # D11: круг типов
            sm.save_settings({"expected_speakers": 3, "enable_diarization": False})
            loaded = sm.load_settings()
            check("D11 круг типов: int->str, bool сохранён",
                  loaded["expected_speakers"] == "3" and loaded["enable_diarization"] is False)
        finally:
            sm.get_writable_base_dir = old  # type: ignore[assignment]

    # D12: save создаёт вложенные несуществующие каталоги
    with tempfile.TemporaryDirectory() as td:
        old = sm.get_writable_base_dir
        try:
            deep = os.path.join(td, "a", "b", "c")
            sm.get_writable_base_dir = lambda: deep  # type: ignore[assignment]
            sm.save_settings({"debug_mode": False})
            check("D12 save создаёт недостающие каталоги",
                  os.path.isfile(os.path.join(deep, "settings.json")))
        finally:
            sm.get_writable_base_dir = old  # type: ignore[assignment]


if __name__ == "__main__":
    test_defaults_file()
    print()
    test_paths()
    print()
    test_normalize()
    print()
    test_load_save()
    print()
    if failures:
        print(f"RESULT: FAIL ({len(failures)}): " + "; ".join(failures[:5]))
        sys.exit(1)
    print("RESULT: PASS")
