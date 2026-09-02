"""Захват кадра экрана через долгоживущую ScreenCast-сессию.

Диалог доступа к экрану показывается только один раз (при создании сессии).
Далее кадры снимаются через уже открытую сессию pipewire+gst-launch, без
повторных запросов. Сессия живёт внутри выделенного потока с собственным
asyncio-циклом, поэтому работает и из Tk-потока GUI, и фоново.

Работает на любом Wayland-композиторе через штатный xdg-desktop-portal.
"""
from __future__ import annotations

import asyncio
import os
import subprocess
import tempfile
import threading
from dataclasses import dataclass
from typing import Callable, TypeVar

from wayland_portal import MONITOR_SOURCE_TYPE
from wayland_portal_async import ScreenCastPortal, PortalError

_T = TypeVar("_T")


@dataclass
class FrameResult:
    """Результат захвата кадра экрана."""

    data: bytes | None = None
    cancelled: bool = False
    error: str | None = None

    @property
    def ok(self) -> bool:
        """True, если кадр получен успешно."""
        return self.data is not None


def _gst_frame(node_id: int, fd: int, out: str) -> str | None:
    """Снимает один кадр через gst-launch-1.0 (pipewiresrc→png): возвращает ошибку или None."""
    import shutil

    launch = shutil.which("gst-launch-1.0")
    if not launch:
        return "gst-launch-1.0 не найден"
    try:
        dup_fd = os.dup(fd)
    except OSError as exc:
        return f"os.dup: {exc}"
    os.set_inheritable(dup_fd, True)
    pipe = (
        f"pipewiresrc fd={dup_fd} path={int(node_id)} num-buffers=1 ! "
        f"queue max-size-buffers=0 max-size-bytes=0 max-size-time=0 ! "
        f"videoconvert ! video/x-raw,format=RGB ! "
        f"pngenc ! filesink location={out}"
    )
    try:
        proc = subprocess.run(
            [launch, "-e", "-q"] + pipe.split(),
            pass_fds=(dup_fd,),
            capture_output=True,
            text=True,
            timeout=15,
        )
    except Exception as exc:
        return f"{type(exc).__name__}: {exc}"
    finally:
        try:
            os.close(dup_fd)
        except OSError:
            pass
    if proc.returncode != 0:
        return (proc.stderr or proc.stdout or f"rc={proc.returncode}").strip()
    return None


class _ScreencastWorker:
    """Держит ScreenCast-сессию в отдельном asyncio-цикле."""

    def __init__(self) -> None:
        """Инициализирует состояние сессии (поток, портал, node_id и флаги готовности)."""
        self._loop: asyncio.AbstractEventLoop | None = None
        self._thread: threading.Thread | None = None
        self._portal: ScreenCastPortal | None = None
        self._session = None
        self._node_id: int | None = None
        self._pipewire_fd: int | None = None
        self._ready = False
        self._cancelled = False
        self._error: str | None = None
        self._cv = threading.Condition(threading.Lock())

    def _ensure_thread(self) -> asyncio.AbstractEventLoop:
        """Гарантирует наличие выделенного фонового потока с работающим asyncio-циклом."""
        if self._loop is not None and self._loop.is_running():
            return self._loop

        def _run():
            self._loop = asyncio.new_event_loop()
            asyncio.set_event_loop(self._loop)
            self._loop.run_forever()

        t = threading.Thread(target=_run, name="screencast-session", daemon=True)
        t.start()
        self._thread = t
        # дождаться запуска цикла
        for _ in range(100):
            if self._loop is not None and self._loop.is_running():
                break
            import time

            time.sleep(0.02)
        return self._loop  # type: ignore[return-value]

    def _call(self, fn: Callable[[], _T], timeout: float = 30.0) -> _T:
        """Синхронно вызывает асинхронную функцию в фоновом цикле с таймаутом."""
        loop = self._ensure_thread()
        future = asyncio.run_coroutine_threadsafe(fn(), loop)
        return future.result(timeout=timeout)

    def ensure(self) -> FrameResult:
        """Создаёт ScreenCast-сессию (один раз) и возвращает состояние/ошибку в FrameResult."""
        if self._ready:
            return FrameResult()
        if self._cancelled:
            return FrameResult(data=None, cancelled=True, error=self._error)
        if self._error:
            return FrameResult(data=None, cancelled=False, error=self._error)

        async def _setup():
            portal = ScreenCastPortal()
            await portal.connect()
            session = await portal.create_session(persist=False)
            await portal.select_sources(
                session, types=MONITOR_SOURCE_TYPE, multiple=False, cursor_mode=2
            )
            streams = await portal.start(session, parent_window="")
            if not streams:
                raise RuntimeError("ScreenCast не вернул потоки")
            node_id = int(streams[0].node_id)
            raw_fd = await portal.open_pipewire_remote(session)
            self._portal = portal
            self._session = session
            self._node_id = node_id
            self._pipewire_fd = raw_fd
            self._ready = True

        try:
            self._call(_setup)
            return FrameResult()
        except PortalError as exc:
            if "отменено" in str(exc) or "отменен" in str(exc):
                self._cancelled = True
                self._error = str(exc)
                return FrameResult(data=None, cancelled=True, error=str(exc))
            self._error = str(exc)
            return FrameResult(data=None, cancelled=False, error=str(exc))
        except Exception as exc:
            self._error = f"{type(exc).__name__}: {exc}"
            return FrameResult(data=None, cancelled=False, error=self._error)

    def grab(self) -> FrameResult:
        """Снимает свежий кадр через сессию и возвращает PNG-байты (или ошибку/отмену) в FrameResult."""
        res = self.ensure()
        if res.error or res.cancelled:
            return res
        if self._pipewire_fd is None or self._node_id is None:
            return FrameResult(data=None, cancelled=False, error="сессия не готова")
        with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as tmp:
            png_path = tmp.name
        try:
            err = _gst_frame(self._node_id, self._pipewire_fd, png_path)
            if err or not os.path.exists(png_path):
                return FrameResult(
                    data=None,
                    cancelled=self._cancelled,
                    error=err or "PNG не создан",
                )
            with open(png_path, "rb") as fh:
                return FrameResult(data=fh.read())
        finally:
            try:
                os.unlink(png_path)
            except OSError:
                pass


#: Единственный рабочий поток с сессией на процесс.
_worker: _ScreencastWorker | None = None
_worker_lock = threading.Lock()


def grab_screencast_frame() -> FrameResult:
    """Синхронный захват свежего кадра через переиспользуемую сессию.

    Диалог доступа появляется только при первом вызове за процесс.
    """
    global _worker
    with _worker_lock:
        if _worker is None:
            _worker = _ScreencastWorker()
        worker = _worker
    return worker.grab()


def get_shared_screencast_stream() -> tuple[int, int] | None:
    """(node_id, pipewire_fd) уже открытой сессии — для записи видео.

    Возвращает None, если сессия не создана (диалог ещё не показан) либо
    пользователь отменил доступ. Используется для записи всего экрана,
    чтобы не показывать повторное разрешение.
    """
    global _worker
    with _worker_lock:
        if _worker is None:
            _worker = _ScreencastWorker()
        worker = _worker
    if not worker._ready or worker._cancelled:
        return None
    if worker._node_id is None or worker._pipewire_fd is None:
        return None
    return (worker._node_id, worker._pipewire_fd)


def clear_session() -> None:
    """Сбросить кэш сессии (используется при смене источника/выходе)."""
    global _worker
    with _worker_lock:
        _worker = None
