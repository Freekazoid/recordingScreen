"""Помощники путей для frozen-сборок (AppImage/PyInstaller) и dev-запусков."""
import os
import sys


# Префиксы имён каталогов, которые runtime создаёт во временном каталоге:
#   _MEI*                — распаковка PyInstaller onefile;
#   .mount_*             — точка монтирования squashfs AppImage;
#   appimage_extracted_* — распаковка AppImage при extract-and-run (нет FUSE).
_EPHEMERAL_DIR_PREFIXES = ("_MEI", ".mount_", "appimage_extracted_")


def _is_pyinstaller_temp_dir(path: str) -> bool:
    """True, если *path* — временный (недолговечный) каталог frozen-сборки.

    Такой каталог доступен для записи, но удаляется при завершении процесса
    (onefile PyInstaller, маунт AppImage, AppImage extract-and-run), поэтому
    его НЕЛЬЗЯ использовать для постоянных данных (модели, настройки): иначе
    настройки будут теряться после перезапуска.

    Проверяем КАЖДЫЙ компонент пути, а не только последний: при extract-and-run
    AppImage распаковывается в ``<temp>/appimage_extracted_*``, и
    ``sys.executable`` лежит глубже (``.../usr/bin/ScreenRecorder``), поэтому
    одного basename недостаточно.
    """
    for part in os.path.normpath(path).split(os.sep):
        for prefix in _EPHEMERAL_DIR_PREFIXES:
            if part.startswith(prefix) and len(part) > len(prefix):
                return True
    return False


def _macos_bundle_parent(exe_dir: str) -> str:
    """Поднимается из .../Foo.app/Contents/MacOS в каталог, содержащий .app.

    На macOS sys.executable лежит внутри бандла (Contents/MacOS); писать
    туда данные нельзя (ломает подпись/нотаризацию и скрывает файлы),
    поэтому «рядом с программой» — это папка, где лежит сам .app.
    Если структура бандла не распознана — возвращает exe_dir без изменений.
    """
    if os.path.basename(exe_dir) != "MacOS":
        return exe_dir
    contents = os.path.dirname(exe_dir)
    if os.path.basename(contents) != "Contents":
        return exe_dir
    bundle = os.path.dirname(contents)
    if os.path.basename(bundle).endswith(".app"):
        return os.path.dirname(bundle)
    return bundle


def _program_dir() -> str:
    """Каталог, в котором лежит запущенная программа.

    Dev-режим — каталог проекта (где лежит src/). Frozen-режим — каталог
    исполняемого файла; на macOS это папка, содержащая .app-бандл.
    """
    if getattr(sys, "frozen", False):
        exe_dir = os.path.dirname(os.path.abspath(sys.executable))
        if sys.platform == "darwin":
            return _macos_bundle_parent(exe_dir)
        return exe_dir
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def get_writable_base_dir() -> str:
    """Каталог для постоянных данных приложения: settings.json, model/, .hf_token.

    Реальный AppImage запускается из read-only маунта squashfs (/tmp/.mount_*),
    поэтому каталог рядом с исполняемым файлом нельзя записывать. Предпочитаем
    его, когда он доступен для записи (обычные сборки PyInstaller / переносимый
    каталог), иначе переходим на пользовательский XDG-каталог данных.

    Работает одинаково на Linux, Windows и macOS (для последней — папка,
    содержащая .app-бандл, а не внутренности бандла).
    """
    if not getattr(sys, "frozen", False):
        return _program_dir()
    app_dir = _program_dir()
    if os.access(app_dir, os.W_OK) and not _is_pyinstaller_temp_dir(app_dir):
        return app_dir
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
