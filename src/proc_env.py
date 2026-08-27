"""Чистое окружение для дочерних процессов вне бандла.

Замороженное приложение (PyInstaller) добавляет свой каталог ``_internal``
в ``LD_LIBRARY_PATH``. Внешние программы, запускаемые из приложения
(systemd-run, pactl, ffmpeg и т.п.), начинают находить библиотеки из
бандла раньше системных и падают с несовпадением версий символов,
например::

    libcrypto.so.3: version `OPENSSL_3.4.0' not found
    (required by /usr/lib/.../libsystemd-shared-259.so)

:func:`bundle_free_env` возвращает копию окружения без таких путей.
:func:`install_subprocess_guard` подключает очистку ко всем вызовам
модуля ``subprocess`` сразу (Popen/run/check_output/check_call).
Для ``os.exec*`` очищенное окружение передаётся явно через ``os.execve``.

Вне заморозки (запуск из исходников) очистка — no-op: окружение уже
не содержит путей бандла.
"""

from __future__ import annotations

import os
import sys

_LD_VARS = ("LD_LIBRARY_PATH", "LD_PRELOAD", "LD_AUDIT")


def _bundle_roots() -> list[str]:
    """Каталоги бандла, пути которых нельзя отдавать внешним процессам."""
    roots: list[str] = []
    meipass = getattr(sys, "_MEIPASS", None)
    if meipass:
        roots.append(os.path.realpath(meipass))
    try:
        exe_dir = os.path.dirname(os.path.realpath(sys.executable))
        roots.append(exe_dir)
        roots.append(os.path.join(exe_dir, "_internal"))
    except Exception:
        pass
    return [r for r in roots if r]


def _from_bundle(path: str, roots: list[str]) -> bool:
    if not path:
        return False
    real = os.path.realpath(path)
    return any(real == root or real.startswith(root + os.sep) for root in roots)


def bundle_free_env(env: dict[str, str] | None = None) -> dict[str, str]:
    """Копия окружения без путей бандла в переменных компоновщика."""
    result = dict(os.environ if env is None else env)
    if not getattr(sys, "frozen", False):
        return result
    roots = _bundle_roots()
    for var in _LD_VARS:
        value = result.get(var, "")
        if not value:
            continue
        parts = value.split(":")
        kept = [p for p in parts if not _from_bundle(p, roots)]
        if kept != parts:
            if kept:
                result[var] = ":".join(kept)
            else:
                result.pop(var, None)
    return result


_guard_installed = False


def install_subprocess_guard() -> None:
    """Прочистить окружение для всех последующих запусков через subprocess.

    На Windows добавляет CREATE_NO_WINDOW ко всем Popen-вызовам, чтобы
    консольные окна ffmpeg и других утилит не открывались при записи/
    обработке.

    Явно переданный вызывающим кодом ``env=`` никуда не пропадает: он
    накладывается поверх очищенной базы, и его значения приоритетны.
    Повторный вызов ни на что не влияет.
    """
    global _guard_installed
    if _guard_installed:
        return

    def _merged(user_env):
        base = bundle_free_env()
        if user_env is None:
            return base
        merged = dict(base)
        merged.update(user_env)
        return merged

    import subprocess

    _CREATE_NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0
    orig_init = subprocess.Popen.__init__

    def patched_init(self, *args, **kwargs):  # type: ignore[no-untyped-def]
        kwargs["env"] = _merged(kwargs.get("env"))
        if _CREATE_NO_WINDOW:
            flags = kwargs.get("creationflags", 0)
            kwargs["creationflags"] = flags | _CREATE_NO_WINDOW
        orig_init(self, *args, **kwargs)

    subprocess.Popen.__init__ = patched_init  # type: ignore[method-assign]
    _guard_installed = True
