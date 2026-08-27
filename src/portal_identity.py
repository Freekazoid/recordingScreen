"""Идентификация приложения для порталов freedesktop.

GNOME 46+ показывает диалоги доступа к экрану только приложениям с
идентификатором, а разрешение привязывает к этому идентификатору.
Идентификатор берётся из имени systemd-scope вида
``app-<app-id>-<число>.scope``, в котором запущен процесс. Обычный запуск
из терминала даёт пустой идентификатор — тогда портал мгновенно отвечает
отказом без всякого диалога.

Решение: при запуске на Wayland вне такого scope перезапускаем сами себя в
корректно названном transient-scope (то же самое делает gnome-shell, когда
приложение запускают из меню). Это не меняет настройки системы — scope
существует только пока работает приложение.
"""

from __future__ import annotations

import os
import re
import shutil
import sys

from logging_utils import write_error_report

APP_ID = "io.github.freekazoid.recordingscreen"

_SCOPE_RE = re.compile(r"/app-(?P<id>[A-Za-z0-9.]+(?:_[A-Za-z0-9.]+)?)-\d+\.scope$")


def _dbg(msg: str) -> None:
    try:
        write_error_report("area_select", msg)
    except Exception:
        pass


def current_scope_app_id() -> str | None:
    """App-id из имени текущего systemd-scope либо None."""
    try:
        with open("/proc/self/cgroup", encoding="utf-8") as fh:
            cgroup = fh.read().strip()
    except OSError:
        return None
    match = _SCOPE_RE.search(cgroup)
    if not match:
        return None
    app_id = match.group("id")
    # Валидный desktop-id обязан содержать точку (reverse-DNS)
    if "." not in app_id:
        return None
    return app_id


def ensure_portal_identity() -> None:
    """Перезапустить себя в scope ``app-<APP_ID>-<pid>.scope`` при необходимости.

    Ничего не делает: вне Wayland, под заморозкой не требуется проверять
    дважды, если идентификатор уже корректен или systemd-run недоступен.
    """
    if sys.platform not in ("linux", "linux2"):
        return
    if os.environ.get("SCREENRECORDER_NO_REEXEC"):
        return
    current = current_scope_app_id()
    if current is not None:
        _dbg(f"identity: уже в scope с app-id={current}")
        return

    systemd_run = shutil.which("systemd-run")
    if systemd_run is None:
        _dbg("identity: systemd-run не найден — работаем с текущим app-id")
        return

    argv = [
        systemd_run,
        "--user",
        "--scope",
        "-q",
        f"--unit=app-{APP_ID}-{os.getpid()}",
        "--setenv=SCREENRECORDER_NO_REEXEC=1",
        sys.executable,
        *sys.argv,
    ]
    _dbg(f"identity: перезапуск в scope app-{APP_ID}: {argv}")
    try:
        # execve (а не execvp): окружение очищается от путей бандла,
        # иначе systemd-run подхватывает libcrypto из _internal и падает.
        from proc_env import bundle_free_env

        os.execve(argv[0], argv, bundle_free_env())
    except Exception as exc:
        _dbg(f"identity: перезапуск не удался ({type(exc).__name__}: {exc}) — продолжаем как есть")
