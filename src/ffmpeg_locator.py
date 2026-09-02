import os
import shutil
import sys
import stat
import tarfile
import tempfile
import urllib.request
import zipfile
from typing import Callable


# Платформенные URL статических сборок ffmpeg (по умолчанию; не
# переопределяются в экзе, т.к. экзе уже содержит ffmpeg).
_WINDOWS_ZIP = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
_LINUX_TAR = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz"
_MACOS_ZIP = "https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip"

_platform_dir = (
    "windows" if sys.platform == "win32"
    else "linux" if sys.platform.startswith("linux")
    else "macos"
)


def _binary_name(tool: str) -> str:
    """Возвращает имя исполняемого файла с суффиксом .exe на Windows."""
    return f"{tool}.exe" if sys.platform == "win32" else tool


def _project_root() -> str:
    """Возвращает корень проекта: рядом с исполняемым файлом (frozen) или каталог выше."""
    if getattr(sys, "frozen", False):
        return os.path.dirname(sys.executable)
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _candidate_dirs() -> list[str]:
    """Возвращает уникальные каталоги-кандидаты для поиска ffmpeg/ffprobe."""
    dirs: list[str] = []
    root = _project_root()
    dirs.append(root)
    dirs.append(os.path.join(root, "bin"))

    platform_dir = "windows" if sys.platform == "win32" else "linux" if sys.platform.startswith("linux") else "macos"
    dirs.append(os.path.join(root, "bin", platform_dir))

    meipass = getattr(sys, "_MEIPASS", None)
    if meipass:
        dirs.append(meipass)
        dirs.append(os.path.join(meipass, "bin"))

    unique_dirs: list[str] = []
    seen = set()
    for d in dirs:
        if not d:
            continue
        path = os.path.abspath(d)
        if path in seen:
            continue
        seen.add(path)
        unique_dirs.append(path)
    return unique_dirs


def resolve_tool(tool: str) -> str:
    """Ищет инструмент в каталогах-кандидатах, затем в PATH; иначе возвращает его имя."""
    binary = _binary_name(tool)

    for d in _candidate_dirs():
        candidate = os.path.join(d, binary)
        if os.path.isfile(candidate):
            return candidate

    from_path = shutil.which(binary)
    if from_path:
        return from_path

    return binary


def has_tool(tool: str) -> bool:
    """Проверяет наличие инструмента в файловой системе или PATH."""
    resolved = resolve_tool(tool)
    if os.path.isabs(resolved):
        return os.path.isfile(resolved)
    return shutil.which(resolved) is not None


def ffmpeg_command() -> str:
    """Возвращает путь к ffmpeg или его имя."""
    return resolve_tool("ffmpeg")


def ffprobe_command() -> str:
    """Возвращает путь к ffprobe или его имя."""
    return resolve_tool("ffprobe")


def has_ffmpeg() -> bool:
    """Проверяет доступность ffmpeg."""
    return has_tool("ffmpeg")


def writable_bin_dir() -> str:
    """Каталог для доустановки ffmpeg: bin/<platform> рядом с приложением.

    Берём первый writable-каталог из путей модуля (bin/<platform>,
    затем bin). В frozen-режиме это каталог рядом с исполняемым файлом.
    """
    for d in _candidate_dirs():
        platform_specific = os.path.join(_project_root(), "bin", _platform_dir)
        best = platform_specific if d in (platform_specific, os.path.join(_project_root(), "bin")) else d
        if not best:
            continue
        path = os.path.abspath(best)
        if path.endswith("bin") or path.endswith(f"bin{os.sep}{_platform_dir}"):
            try:
                os.makedirs(path, exist_ok=True)
            except OSError:
                continue
            if os.access(path, os.W_OK):
                return path
    fallback = os.path.join(_project_root(), "bin", _platform_dir)
    os.makedirs(fallback, exist_ok=True)
    return fallback


def _download(url: str, progress: Callable[[int, int], None] | None = None) -> bytes:
    """Скачивает содержимое URL и возвращает его как байты."""
    def _hook(blocks, block_size, total) -> None:
        """Callback для отслеживания прогресса загрузки."""
        if progress and total:
            progress(blocks * block_size, total)
    with urllib.request.urlopen(url, timeout=60) as resp:
        total = int(resp.headers.get("Content-Length", 0))
        data = resp.read()
    return data


def _extract_ffmpeg_family(archive_path: str, dest_dir: str) -> None:
    """Извлечь ffmpeg и ffprobe из архива в dest_dir (поиск по имени)."""
    exe_suffix = ".exe" if _platform_dir == "windows" else ""
    # Точное имя исполняемого файла (с учётом суффикса платформы).
    allow = {"ffmpeg", "ffprobe"}
    if exe_suffix:
        allow = {f"{t}{exe_suffix}" for t in allow}
    if archive_path.endswith(".zip"):
        with zipfile.ZipFile(archive_path) as z:
            for name in z.namelist():
                base = os.path.basename(name)
                if base in allow:
                    with z.open(name) as src, open(os.path.join(dest_dir, base), "wb") as dst:
                        shutil.copyfileobj(src, dst)
    elif archive_path.endswith((".tar.xz", ".tar.gz", ".txz", ".tgz")):
        with tarfile.open(archive_path, "r:*") as t:
            for member in t.getmembers():
                base = os.path.basename(member.name)
                base = base.replace(exe_suffix, "").split(".")[0]
                if member.isfile() and base in {"ffmpeg", "ffprobe"}:
                    out_name = base + exe_suffix
                    with t.extractfile(member) as src, open(os.path.join(dest_dir, out_name), "wb") as dst:
                        shutil.copyfileobj(src, dst)


def _default_ffmpeg_url() -> str:
    """Возвращает URL статической сборки ffmpeg для текущей платформы."""
    return {
        "windows": _WINDOWS_ZIP,
        "linux": _LINUX_TAR,
        "macos": _MACOS_ZIP,
    }[_platform_dir]


def ensure_ffmpeg(progress: Callable[[int, int], None] | None = None) -> bool:
    """Гарантирует наличие ffmpeg.

    Если ffmpeg уже найден (в системе или рядом с приложением) — True.
    Иначе скачивает статическую сборку в bin/<platform> и возвращает True.
    При любом сбое — False и сообщение в progress(0,0).
    """
    if has_ffmpeg():
        return True

    dest = writable_bin_dir()
    if has_ffmpeg():
        # стало доступно после создания каталога (маловероятно, но безопасно)
        return True

    url = _default_ffmpeg_url()
    d = tempfile.NamedTemporaryFile(suffix=".zip" if _platform_dir == "windows" else (".zip" if _platform_dir == "macos" else ".tar.xz"), delete=False)
    archive_path = d.name
    d.close()
    try:
        data = _download(url, progress)
        with open(archive_path, "wb") as f:
            f.write(data)
        _extract_ffmpeg_family(archive_path, dest)
    except Exception as exc:
        if progress:
            progress(0, 0)
        _cleanup(archive_path)
        raise RuntimeError(f"Не удалось скачать ffmpeg: {exc}") from exc
    finally:
        _cleanup(archive_path)

    # Бинарнику нужен exec-бит на POSIX.
    if _platform_dir != "windows":
        for tool in ("ffmpeg", "ffprobe"):
            p = os.path.join(dest, tool)
            if os.path.exists(p):
                os.chmod(p, os.stat(p).st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)

    return has_ffmpeg()


def _cleanup(path: str) -> None:
    """Пытается удалить файл, игнорируя ошибки."""
    try:
        if os.path.exists(path):
            os.unlink(path)
    except OSError:
        pass
