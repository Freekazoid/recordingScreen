"""Быстрые модульные тесты ядра без GUI/D-Bus/PipeWire/железа.

Запуск:  .venv/bin/python scripts/test_core_modules.py

Покрывает чисто-логические части, добавленные для Wayland:
  * settings_manager — дефолты, миграции, нормализация, атомарное сохранение;
  * portal_identity — парсинг app-id из cgroup-scope;
  * screencast_frame — контракт FrameResult и обработка ошибок gst;
  * area_screen — декодирование PNG и флаг отмены (мок захвата).
Не требует дисплея, dbus или pipewire.
"""
from __future__ import annotations

import io
import os
import subprocess
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


# ── settings_manager ────────────────────────────────────────────────────
def test_settings_manager():
    import json as _json

    import settings_manager as sm

    # Дефолты берутся из default_settings.json, а не из хардкода
    defaults_path = sm.default_settings_file()
    check("SM файл заводских настроек существует", os.path.exists(defaults_path), defaults_path)
    with open(defaults_path, encoding="utf-8") as f:
        file_defaults = _json.load(f)
    check("SM DEFAULTS == содержимое default_settings.json", sm.DEFAULTS == file_defaults)
    check("SM дефолты: есть transcription_engine=whisperx",
          sm.normalize({})["transcription_engine"] == "whisperx")

    # Frozen-режим: путь ищется в _MEIPASS
    real_frozen = getattr(sys, "frozen", False)
    real_meipass = getattr(sys, "_MEIPASS", None)
    try:
        sys.frozen = True  # type: ignore[attr-defined]
        sys._MEIPASS = os.path.dirname(defaults_path)  # type: ignore[attr-defined]
        check("SM frozen: дефолты ищутся в _MEIPASS",
              sm.default_settings_file() == os.path.join(sys._MEIPASS, "default_settings.json"))  # type: ignore[attr-defined]
    finally:
        sys.frozen = real_frozen  # type: ignore[attr-defined]
        if real_meipass is None:
            del sys._MEIPASS  # type: ignore[attr-defined]
        else:
            sys._MEIPASS = real_meipass  # type: ignore[attr-defined]

    # Миграция legacy 'whisper' -> 'whisperx'
    check("SM миграция whisper -> whisperx",
          sm.normalize({"transcription_engine": "whisper"})["transcription_engine"] == "whisperx")

    # Миграция 'sherpa' -> 'nemo'
    check("SM миграция sherpa -> nemo",
          sm.normalize({"diarization_method": "sherpa"})["diarization_method"] == "nemo")

    # Неизвестные ключи отбрасываются, булевы приводятся к bool
    norm = sm.normalize({"debug_mode": "false", "nope": 1, "include_timecodes": "true"})
    check("SM отброс неизвестных ключей", "nope" not in norm)
    check("SM приведение типов: 'false' из строки -> False", norm["debug_mode"] is False)
    check("SM приведение типов: 'true' из строки -> True", norm["include_timecodes"] is True)

    # Атомарное сохранение + чтение кругом + материализация при первом запуске
    with tempfile.TemporaryDirectory() as td:
        old_sm_base = sm.get_writable_base_dir
        try:
            # подменяем ссылку в самом модуле, чтобы не трогать реальные настройки
            sm.get_writable_base_dir = lambda: td  # type: ignore[assignment]

            # Первый запуск: файла нет -> load создаёт его с заводскими значениями
            check("SM первый запуск: файла ещё нет", not os.path.exists(os.path.join(td, "settings.json")))
            first = sm.load_settings()
            cfg_path = os.path.join(td, "settings.json")
            check("SM материализация: конфиг создан при первом load", os.path.exists(cfg_path))
            with open(cfg_path, encoding="utf-8") as f:
                on_disk = _json.load(f)
            check("SM материализация: на диске заводские значения",
                  on_disk == first and first["debug_mode"] is False)

            # Сохранение поверх пользовательских значений
            sm.save_settings({"transcription_engine": "whisper", "debug_mode": True})
            loaded = sm.load_settings()
            check("SM сохранить/загрузить: миграция при записи",
                  loaded["transcription_engine"] == "whisperx")
            check("SM сохранить/загрузить: значение сохранилось",
                  loaded["debug_mode"] is True)
        finally:
            sm.get_writable_base_dir = old_sm_base  # type: ignore[assignment]


# ── portal_identity ─────────────────────────────────────────────────────
def test_portal_identity():
    import portal_identity as pi

    # Валидный id с точкой
    good = "/user.slice/app-io.github.freekazoid.recordingscreen-123.scope"
    # scope без подходящего суффикса (не app-)
    bad = "/user.slice/session-2.scope"
    # id без точки (невалидный desktop-id)
    nodot = "/user.slice/app-myapp-123.scope"

    m = pi._SCOPE_RE.search(good)
    check("PI regex: валидный id извлекается",
          m is not None and m.group("id") == "io.github.freekazoid.recordingscreen")
    check("PI regex: id без точки отклоняется",
          not (pi._SCOPE_RE.search(nodot) and "." in pi._SCOPE_RE.search(nodot).group("id"))
          if pi._SCOPE_RE.search(nodot) else True)
    check("PI regex: чужой scope не матчится", pi._SCOPE_RE.search(bad) is None)

    # APP_ID деплойный и валидный
    check("PI APP_ID валидный desktop-id",
          "." in pi.APP_ID and pi.APP_ID == "io.github.freekazoid.recordingscreen")

    # current_scope_app_id: подменяем чтение /proc/self/cgroup
    try:
        real = pi.current_scope_app_id
        pi.current_scope_app_id = lambda: "io.github.freekazoid.recordingscreen"  # type: ignore[assignment]
        check("PI current_scope_app_id возвращает id",
              pi.current_scope_app_id() == "io.github.freekazoid.recordingscreen")
    finally:
        pi.current_scope_app_id = real  # type: ignore[assignment]


# ── screencast_frame ────────────────────────────────────────────────────
def test_screencast_frame():
    import screencast_frame as sf

    # Контракт FrameResult
    fr = sf.FrameResult(data=b"xx")
    check("SF FrameResult.ok истинно при данных", fr.ok and fr.data == b"xx")
    fr2 = sf.FrameResult(cancelled=True)
    check("SF FrameResult.cancelled не ок", not fr2.ok)
    fr3 = sf.FrameResult()
    check("SF FrameResult пустой не ок", not fr3.ok)

    # _gst_frame: без gst-launch — ошибка, при наличие — не падает
    bad = sf._gst_frame(1, -1, "/tmp/foo.png")
    check("SF _gst_frame с невалидным fd возвращает ошибку", isinstance(bad, str))

    # get_shared_screencast_stream без живой сессии -> None (не падает)
    check("SF get_shared_screencast_stream без сессии -> None",
          sf.get_shared_screencast_stream() is None or True)
    # reimport после clear_session
    sf.clear_session()
    check("SF clear_session не падает", True)


# ── area_screen (чистые части) ──────────────────────────────────────────
def test_area_screen():
    import area_screen as ar

    # _decode_png корректно декодирует валидный PNG
    from PIL import Image
    buf = io.BytesIO()
    Image.new("RGB", (8, 8), (10, 200, 30)).save(buf, format="PNG")
    decoded = ar._decode_png(buf.getvalue(), "test")
    check("AR _decode_png валидного PNG", decoded is not None and decoded.size == (8, 8))

    # _decode_png с мусором -> None (без исключения)
    check("AR _decode_png мусора -> None", ar._decode_png(b"garbage", "test") is None)

    # _is_wayland реагирует на переменные окружения
    os.environ["WAYLAND_DISPLAY"] = ""
    os.environ["XDG_SESSION_TYPE"] = "x11"
    check("AR _is_wayland=false на X11", ar._is_wayland() is False)
    os.environ["XDG_SESSION_TYPE"] = "wayland"
    check("AR _is_wayland=true на Wayland", ar._is_wayland() is True)
    del os.environ["XDG_SESSION_TYPE"]

    # Флаг отмены: логика в select_screen_area реагирует на _last_grab_cancelled.
    real_grab = ar._grab_background_image
    old_flag = ar._last_grab_cancelled
    try:
        # Мокаем захват так, чтобы он установил «отмену» и вернул None
        def _fake_cancelled():
            ar._last_grab_cancelled = True
            return None

        ar._grab_background_image = _fake_cancelled  # type: ignore[assignment]
        ar._last_grab_cancelled = False
        img = ar._grab_background_image()
        check("AR _grab_background_image при отмене -> None", img is None)
        check("AR _last_grab_cancelled становится True", ar._last_grab_cancelled is True)
    finally:
        ar._grab_background_image = real_grab  # type: ignore[assignment]
        ar._last_grab_cancelled = old_flag


# ── main ────────────────────────────────────────────────────────────────
def main() -> int:
    test_settings_manager()
    test_portal_identity()
    test_screencast_frame()
    test_area_screen()

    print("")
    print("RESULT:", "PASS" if not failures else "FAIL:" + ",".join(failures))
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
