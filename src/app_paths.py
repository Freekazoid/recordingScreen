"""Path helpers for frozen (AppImage/PyInstaller) and development runs."""
import os
import sys


def _is_pyinstaller_temp_dir(path: str) -> bool:
    """True if *path* is a PyInstaller onefile extraction dir (e.g. _MEI00012345).

    On Windows PyInstaller onefile, sys.executable points to the extracted
    copy inside %TEMP%/_MEI*.  This directory is writable but ephemeral
    (deleted when the process exits), so it must NOT be used for persistent
    data like models or settings.
    """
    base = os.path.basename(path)
    if base.startswith("_MEI") and len(base) > 4:
        return True
    # AppImage squashfs mount: /tmp/.mount_*
    if base.startswith(".mount_"):
        return True
    return False


def get_writable_base_dir() -> str:
    """Directory for persistent app data: settings.json, model/, .hf_token.

    A real AppImage runs from a read-only squashfs mount (/tmp/.mount_*),
    so the directory next to the executable cannot be written. Prefer it
    when writable (plain PyInstaller builds), otherwise fall back to the
    XDG data directory.
    """
    if not getattr(sys, "frozen", False):
        return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    exe_dir = os.path.dirname(os.path.abspath(sys.executable))
    if os.access(exe_dir, os.W_OK) and not _is_pyinstaller_temp_dir(exe_dir):
        return exe_dir
    xdg_data = os.environ.get("XDG_DATA_HOME") or os.path.join(
        os.path.expanduser("~"), ".local", "share"
    )
    return os.path.join(xdg_data, "ScreenRecorder")


def safe_timestamp(fmt: str = "%d.%m.%Y - %H.%M") -> str:
    """Отметка времени, безопасная для имён файлов на Windows.

    Обычный strftime-формат ``%H:%M`` содержит двоеточие ``:``, которое
    Windows трактует как начало Alternate Data Stream, а не как часть
    имени файла. Вместо него используем точку, которая безопасна.
    """
    from datetime import datetime

    sanitized = fmt.replace("%H:%M", "%H.%M").replace(":%S", ".%S")
    return datetime.now().strftime(sanitized)


__all__ = ["get_writable_base_dir", "ensure_writable_base_dir", "safe_timestamp"]


def ensure_writable_base_dir() -> str:
    """get_writable_base_dir() + создание каталога при отсутствии."""
    base = get_writable_base_dir()
    try:
        os.makedirs(base, exist_ok=True)
    except OSError as e:
        print(f"Не удалось создать каталог данных {base}: {e}")
    return base
