from __future__ import annotations

import asyncio
import os
import shutil
import signal
import subprocess
import sys
import threading
import traceback
from typing import Any

from ffmpeg_pipewire import (
    FFmpegNotFoundError,
    FFmpegPipewireProcess,
    GstLaunchPipewireProcess,
    GstPipewireProcess,
    RecordingOptions,
    gst_launch_pipewire_available,
    gst_pipewire_available,
    pipewire_demuxer_available,
    pipewire_recording_backend_available,
)
from logging_utils import write_error_report
from wayland_portal_async import (
    PortalError,
    ScreenCastPortal,
    dbus_available,
    dbus_dependency_error,
    dbus_supports_unix_fd,
)


def _import_wayland_portal():
    """Import GI portal helpers without permanently polluting sys.path.

    Permanently adding /usr/lib/python*/dist-packages into a frozen AppImage
    makes Python pick the system psutil (often built for another Python ABI)
    and breaks Whisper with `_psutil_linux` / circular import errors.
    """
    added: list[str] = []
    if getattr(sys, "frozen", False):
        for path in (
            f"/usr/lib/python{sys.version_info.major}.{sys.version_info.minor}/dist-packages",
            "/usr/lib/python3/dist-packages",
        ):
            if os.path.isdir(path) and path not in sys.path:
                sys.path.insert(0, path)
                added.append(path)
    try:
        from wayland_portal import (
            MONITOR_SOURCE_TYPE,
            WINDOW_SOURCE_TYPE,
            WaylandPortalError,
            WaylandPortalSession,
            portal_dependencies_available,
        )
        return (
            MONITOR_SOURCE_TYPE,
            WINDOW_SOURCE_TYPE,
            WaylandPortalError,
            WaylandPortalSession,
            portal_dependencies_available,
        )
    except Exception:
        return None
    finally:
        for path in added:
            try:
                sys.path.remove(path)
            except ValueError:
                pass
        if added:
            for mod in list(sys.modules):
                if mod == "psutil" or mod.startswith("psutil."):
                    del sys.modules[mod]


_portal_import = _import_wayland_portal()
if _portal_import is not None:
    (
        MONITOR_SOURCE_TYPE,
        WINDOW_SOURCE_TYPE,
        WaylandPortalError,
        WaylandPortalSession,
        portal_dependencies_available,
    ) = _portal_import
else:
    MONITOR_SOURCE_TYPE = 1
    WINDOW_SOURCE_TYPE = 2
    WaylandPortalError = RuntimeError  # type: ignore
    WaylandPortalSession = None  # type: ignore

    def portal_dependencies_available() -> bool:
        return False


class WaylandPortalRecorderError(RuntimeError):
    pass


def is_wayland_session() -> bool:
    session_type = os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland"
    wayland_socket = bool(os.environ.get("WAYLAND_DISPLAY"))
    return session_type or wayland_socket


def _recording_backend_available() -> bool:
    """True when we can capture PipeWire after the portal returns a stream."""
    return pipewire_recording_backend_available()


def _portal_dbus_next_backend_available() -> bool:
    return dbus_available() and dbus_supports_unix_fd() and _recording_backend_available()


def _portal_gi_backend_available() -> bool:
    return bool(WaylandPortalSession) and portal_dependencies_available() and _recording_backend_available()


def _wf_backend_available() -> bool:
    return shutil.which("wf-recorder") is not None


def _likely_wlroots_compositor() -> bool:
    desktop = (os.environ.get("XDG_CURRENT_DESKTOP", "") + "," + os.environ.get("DESKTOP_SESSION", "")).lower()
    markers = ("sway", "hypr", "river", "wayfire", "labwc", "wlroots")
    return any(marker in desktop for marker in markers)


def wayland_dependency_issue() -> str | None:
    if not is_wayland_session():
        return "Текущая сессия не Wayland"
    if _portal_dbus_next_backend_available() or _portal_gi_backend_available():
        return None
    if _wf_backend_available() and _likely_wlroots_compositor():
        return None

    issues = []
    if not dbus_available() or not dbus_supports_unix_fd():
        issues.append(f"dbus-next: {dbus_dependency_error()}")
    if not portal_dependencies_available():
        issues.append("PyGObject/GIO portal недоступен")
    if not _recording_backend_available():
        issues.append(
            "нет бэкенда записи PipeWire "
            "(нужен gst-launch-1.0/pipewiresrc или ffmpeg с demuxer pipewire)"
        )
    if shutil.which("wf-recorder") is None:
        issues.append("wf-recorder: не найден")
    elif not _likely_wlroots_compositor():
        issues.append("wf-recorder требует wlroots-композитор")
    return "; ".join(issues) if issues else "backend записи Wayland недоступен"


class WaylandProgramScreenMode:
    def __init__(
        self,
        root,
        on_start_callback,
        fps: int = 20,
        logger: Any | None = None,
        *,
        source_types: int = WINDOW_SOURCE_TYPE,
        selection_label: str = "окно",
        crop: tuple[int, int, int, int] | None = None,
        video_crf: int = 23,
    ):
        self.root = root
        self.on_start_callback = on_start_callback
        self.fps = max(int(fps), 1)
        self.logger = logger
        self._selection_label = selection_label
        self._source_types = source_types
        self._crop = crop
        self._crf = max(1, min(51, video_crf))

        self._lock = threading.Lock()
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()
        self._active = False
        self._last_error: str | None = None
        self._error_logged = False

        self._ffmpeg_proc: FFmpegPipewireProcess | GstPipewireProcess | None = None
        self._wf_proc: subprocess.Popen | None = None
        self._session = None

        self._start_thread()

    @staticmethod
    def is_supported() -> bool:
        return wayland_dependency_issue() is None

    @staticmethod
    def _coerce_number(value) -> float | None:
        if hasattr(value, "unpack"):
            try:
                value = value.unpack()
            except Exception:
                pass
        if isinstance(value, (int, float)):
            return float(value)
        if isinstance(value, str):
            try:
                return float(value)
            except ValueError:
                return None
        return None

    @classmethod
    def _read_pair(cls, value, *, kind: str) -> tuple[float, float] | None:
        if value is None:
            return None
        if isinstance(value, (list, tuple)) and len(value) >= 2:
            first = cls._coerce_number(value[0])
            second = cls._coerce_number(value[1])
            if first is None or second is None:
                return None
            return (first, second)
        if isinstance(value, dict):
            if kind == "pos":
                keys = ("x", "X", "left", "Left")
                keys_y = ("y", "Y", "top", "Top")
            else:
                keys = ("width", "Width", "w", "W")
                keys_y = ("height", "Height", "h", "H")
            x_val = None
            y_val = None
            for key in keys:
                if key in value:
                    x_val = cls._coerce_number(value.get(key))
                    break
            for key in keys_y:
                if key in value:
                    y_val = cls._coerce_number(value.get(key))
                    break
            if x_val is None or y_val is None:
                return None
            return (x_val, y_val)
        return None

    @classmethod
    def _read_rect(cls, value) -> tuple[float, float, float, float] | None:
        if value is None:
            return None
        if isinstance(value, (list, tuple)) and len(value) >= 4:
            x_val = cls._coerce_number(value[0])
            y_val = cls._coerce_number(value[1])
            w_val = cls._coerce_number(value[2])
            h_val = cls._coerce_number(value[3])
            if None in (x_val, y_val, w_val, h_val):
                return None
            return (x_val, y_val, w_val, h_val)  # type: ignore[return-value]
        if isinstance(value, dict):
            pos = cls._read_pair(value, kind="pos")
            size = cls._read_pair(value, kind="size")
            if pos and size:
                return (pos[0], pos[1], size[0], size[1])
        return None

    @classmethod
    def _extract_scale(cls, mapping: dict) -> float | None:
        for key in ("scale", "Scale", "window_scale", "window-scale", "buffer_scale", "buffer-scale"):
            if key in mapping:
                value = cls._coerce_number(mapping.get(key))
                if value and value > 0:
                    return value
        return None

    @classmethod
    def _extract_rect_from_mapping(cls, mapping: dict) -> tuple[int, int, int, int] | None:
        if not isinstance(mapping, dict):
            return None
        scale = cls._extract_scale(mapping) or 1.0

        pos = None
        size = None
        for key in ("position", "Position", "pos", "Pos"):
            if key in mapping:
                pos = cls._read_pair(mapping.get(key), kind="pos")
                if pos:
                    break
        for key in ("size", "Size", "dimensions", "Dimensions"):
            if key in mapping:
                size = cls._read_pair(mapping.get(key), kind="size")
                if size:
                    break

        if pos and size:
            x, y = pos
            w, h = size
        else:
            rect = None
            for key in ("rect", "Rect", "geometry", "Geometry", "window_rect", "window-rect", "bounds", "Bounds"):
                if key in mapping:
                    rect = cls._read_rect(mapping.get(key))
                    if rect:
                        break
            if rect:
                x, y, w, h = rect
            else:
                return None

        x *= scale
        y *= scale
        w *= scale
        h *= scale

        x0 = int(round(x))
        y0 = int(round(y))
        x1 = int(round(x + w))
        y1 = int(round(y + h))
        if x1 - x0 < 2 or y1 - y0 < 2:
            return None
        return (x0, y0, x1, y1)

    @classmethod
    def _stream_properties(cls, stream) -> dict:
        if stream is None:
            return {}
        if isinstance(stream, dict):
            return stream
        props = getattr(stream, "properties", None)
        if isinstance(props, dict):
            return props
        return {}

    @classmethod
    def _stream_source_type(cls, stream) -> int | None:
        props = cls._stream_properties(stream)
        for key in ("source_type", "SourceType", "source-type", "SourceType"):
            if key in props:
                try:
                    return int(props.get(key))  # type: ignore[arg-type]
                except Exception:
                    return None
        return None

    @classmethod
    def _stream_node_id(cls, stream) -> int:
        if stream is None:
            return 0
        if isinstance(stream, dict):
            for key in ("node_id", "node-id", "nodeId"):
                if key in stream:
                    try:
                        return int(stream.get(key))  # type: ignore[arg-type]
                    except Exception:
                        return 0

        node_id = getattr(stream, "node_id", None)
        try:
            return int(node_id)  # type: ignore[arg-type]
        except Exception:
            return 0
    @classmethod
    def _stream_size_area(cls, stream) -> int | None:
        props = cls._stream_properties(stream)
        size = None
        for key in ("size", "Size", "dimensions", "Dimensions"):
            if key in props:
                size = cls._read_pair(props.get(key), kind="size")
                if size:
                    break
        if not size:
            return None
        w, h = size
        if w <= 0 or h <= 0:
            return None
        return int(w * h)

    @classmethod
    def _pick_stream(cls, streams, preferred_source_type: int | None = None):
        if not streams:
            return None
        if not isinstance(streams, (list, tuple)):
            return streams
        if len(streams) == 1:
            return streams[0]

        if preferred_source_type is not None:
            matches = [s for s in streams if cls._stream_source_type(s) == preferred_source_type]
            if matches:
                return min(matches, key=lambda s: cls._stream_size_area(s) or 2**63)

        sized = [s for s in streams if cls._stream_size_area(s) is not None]
        if sized:
            return min(sized, key=lambda s: cls._stream_size_area(s) or 2**63)

        return streams[0]

    def _start_thread(self):
        self._thread = threading.Thread(target=self._worker, daemon=True)
        self._thread.start()

    def _worker(self):
        try:
            asyncio.run(self._worker_async())
        except Exception as exc:
            self._notify_failure(f"Wayland recorder crashed: {exc}")
            self._record_error()
            self._dispatch(lambda: self.on_start_callback(False))

    async def _worker_async(self):
        issue = wayland_dependency_issue()
        if issue:
            self._notify_failure(issue)
            self._record_error()
            self._dispatch(lambda: self.on_start_callback(False))
            return

        errors: list[str] = []

        # Frozen AppImage has no matching system PyGObject — prefer dbus-next.
        # Unpackaged runs on GNOME prefer GI (historically more reliable there).
        prefer_dbus_first = bool(getattr(sys, "frozen", False))
        portal_order = (
            ("dbus-next", _portal_dbus_next_backend_available, self._run_portal_dbus_next_backend),
            ("PyGObject", _portal_gi_backend_available, self._run_portal_gi_backend),
        )
        if not prefer_dbus_first:
            portal_order = (
                ("PyGObject", _portal_gi_backend_available, self._run_portal_gi_backend),
                ("dbus-next", _portal_dbus_next_backend_available, self._run_portal_dbus_next_backend),
            )

        for label, available, runner in portal_order:
            if not available():
                if label == "dbus-next" and dbus_available() and not dbus_supports_unix_fd():
                    msg = dbus_dependency_error()
                    errors.append(f"dbus-next: {msg}")
                    self._log(f"portal-dbus-next пропущен ({msg})")
                continue
            started, err = await runner()
            if started:
                return
            if err:
                errors.append(f"{label}: {err}")
                self._log(f"portal-{label} недоступен, пробуем следующий backend: {err}")

        if _wf_backend_available() and _likely_wlroots_compositor():
            started, err = await self._run_wf_backend()
            if started:
                return
            if err:
                errors.append(f"wf-recorder: {err}")
                self._log(f"wf-recorder недоступен: {err}")

        message = "; ".join(errors) if errors else "backend записи Wayland недоступен"
        self._notify_failure(message)
        self._record_error()
        self._dispatch(lambda: self.on_start_callback(False))

    async def _run_portal_dbus_next_backend(self) -> tuple[bool, str | None]:
        """Run dbus-next portal backend.

        Returns (started, error). started=True means on_start(True) was called
        (or recording finished) and no further backends should be tried.
        """
        portal = ScreenCastPortal()
        session = None
        started = False
        try:
            # Переиспользуем уже открытую сессию доступа к экрану: после
            # выбора области диалог показан один раз, и запись всего экрана
            # может вестись через тот же поток без повторного разрешения.
            if self._source_types == MONITOR_SOURCE_TYPE:
                from screencast_frame import get_shared_screencast_stream

                shared = get_shared_screencast_stream()
                if shared is not None:
                    node_id, fd = shared
                    if node_id > 0:
                        self._log("Переиспользуем открытую сессию доступа к экрану (без нового запроса)")
                        await self._run_ffmpeg_loop(node_id=node_id, fd=fd)
                        started = True
                        self._log("Wayland запись завершена (portal-dbus-next+ffmpeg)")
                        return True, None

            await portal.connect()
            session = await portal.create_session(persist=0)
            self._session = session
            await portal.select_sources(session, types=self._source_types, multiple=False, cursor_mode=2)
            streams = await portal.start(session)
            if not streams:
                raise WaylandPortalRecorderError("Портал не вернул источники записи")

            stream = self._pick_stream(
                streams,
                preferred_source_type=WINDOW_SOURCE_TYPE if self._source_types == WINDOW_SOURCE_TYPE else None,
            )
            if stream is None:
                raise WaylandPortalRecorderError("Портал не вернул корректный поток")

            if self._crop is None and self._source_types == WINDOW_SOURCE_TYPE:
                crop = self._extract_crop_from_stream(stream)
                if crop:
                    self._crop = crop
                    self._log(
                        f"Расположение окна из портала: "
                        f"({crop[0]},{crop[1]}) — ({crop[2]},{crop[3]})"
                    )
                else:
                    self._log(
                        "Портал не сообщил координаты окна — запись без обрезки. "
                        "На wlroots-композиторах поток уже содержит только окно."
                    )

            source_type = self._stream_source_type(stream)
            if (
                self._source_types == WINDOW_SOURCE_TYPE
                and source_type is not None
                and source_type != WINDOW_SOURCE_TYPE
            ):
                self._log(
                    "Портал вернул поток не окна. "
                    "Если запись выглядит как весь экран, используйте режим 'Область'."
                )

            node_id = self._stream_node_id(stream)
            if node_id <= 0:
                node_id = self._extract_node_id(streams)
            fd = await portal.open_pipewire_remote(session)
            await self._run_ffmpeg_loop(node_id=node_id, fd=fd)
            started = True
            self._log("Wayland запись завершена (portal-dbus-next+ffmpeg)")
            return True, None
        except (PortalError, WaylandPortalRecorderError, FFmpegNotFoundError) as exc:
            if started or self._was_recording_started():
                self._notify_failure(str(exc))
                return True, str(exc)
            return False, str(exc)
        except Exception as exc:  # pragma: no cover
            message = f"Не удалось выполнить запись через portal backend: {exc}"
            if started or self._was_recording_started():
                self._notify_failure(message)
                return True, message
            return False, message
        finally:
            with self._lock:
                was_active = self._active or started or self._ffmpeg_proc is not None
                self._active = False
                self._session = None
                self._ffmpeg_proc = None
            if session is not None:
                try:
                    await session.close()
                except Exception:
                    pass
            if was_active:
                self._record_error()
                self._dispatch(lambda: self.on_start_callback(False))

    async def _run_portal_gi_backend(self) -> tuple[bool, str | None]:
        if WaylandPortalSession is None:
            return False, "PyGObject backend недоступен"

        session = None
        started = False
        try:
            session = WaylandPortalSession(logger=self._log, source_types=self._source_types)
            self._session = session
            result = await asyncio.to_thread(session.open_stream)
            if not result:
                raise WaylandPortalRecorderError(f"Выбор источника ({self._selection_label}) отменён")

            streams = result.get("streams")
            stream = self._pick_stream(
                streams,
                preferred_source_type=WINDOW_SOURCE_TYPE if self._source_types == WINDOW_SOURCE_TYPE else None,
            )
            node_id = self._stream_node_id(stream) if stream is not None else 0
            if node_id <= 0:
                node_id = self._extract_node_id(streams)
            if node_id <= 0:
                raise WaylandPortalRecorderError(
                    f"Портал вернул некорректный node_id (streams={streams!r})"
                )

            if stream is not None:
                self._extract_crop_from_streams([stream])
                source_type = self._stream_source_type(stream)
                if (
                    self._source_types == WINDOW_SOURCE_TYPE
                    and source_type is not None
                    and source_type != WINDOW_SOURCE_TYPE
                ):
                    self._log(
                        "Портал вернул поток не окна. "
                        "Если запись выглядит как весь экран, используйте режим 'Область'."
                    )
            else:
                self._extract_crop_from_streams(streams)

            fd = result.get("pipewire_fd")
            if fd is None:
                raise WaylandPortalRecorderError("Портал не вернул PipeWire FD")

            await self._run_ffmpeg_loop(node_id=node_id, fd=int(fd))
            started = True
            self._log("Wayland запись завершена (portal-gi+ffmpeg)")
            return True, None
        except (WaylandPortalError, WaylandPortalRecorderError, FFmpegNotFoundError) as exc:
            if started or self._was_recording_started():
                self._notify_failure(str(exc))
                return True, str(exc)
            return False, str(exc)
        except Exception as exc:  # pragma: no cover
            message = (
                f"Не удалось выполнить запись через PyGObject backend: "
                f"{type(exc).__name__}: {exc!r}\n{traceback.format_exc()}"
            )
            if started or self._was_recording_started():
                self._notify_failure(message)
                return True, message
            return False, message
        finally:
            with self._lock:
                was_active = self._active or started or self._ffmpeg_proc is not None
                self._active = False
                self._session = None
                self._ffmpeg_proc = None
            if session is not None:
                try:
                    session.close()
                except Exception:
                    pass
            if was_active:
                self._record_error()
                self._dispatch(lambda: self.on_start_callback(False))

    def _was_recording_started(self) -> bool:
        with self._lock:
            return bool(self._active or self._ffmpeg_proc is not None or self._wf_proc is not None)

    async def _run_ffmpeg_loop(self, node_id: int, fd: int):
        options = RecordingOptions(
            output="output.mkv",
            fps=self.fps,
            crop=self._crop_to_ffmpeg(self._crop),
            crf=self._crf,
            preset="veryfast",
        )
        if pipewire_demuxer_available():
            proc = FFmpegPipewireProcess(node_id=node_id, fd=fd, options=options)
        elif gst_pipewire_available():
            proc = GstPipewireProcess(node_id=node_id, fd=fd, options=options, log_callback=self._log)  # type: ignore[assignment]
        elif gst_launch_pipewire_available():
            proc = GstLaunchPipewireProcess(node_id=node_id, fd=fd, options=options, log_callback=self._log)  # type: ignore[assignment]
        else:
            raise WaylandPortalRecorderError(
                "Нет доступного бэкенда для записи PipeWire. "
                "Установите gstreamer1.0-pipewire (gst-launch-1.0) "
                "или ffmpeg с demuxer pipewire."
            )
        proc.start()
        with self._lock:
            self._ffmpeg_proc = proc
            self._active = True
            self._last_error = None
        self._dispatch(lambda: self.on_start_callback(True))

        while not self._stop_event.is_set():
            if proc.wait(timeout=0.25):
                break

        if self._stop_event.is_set():
            proc.stop()
        else:
            proc.wait()

        import os
        if os.path.exists("output.mkv"):
            self._log(f"LOGS: output.mkv exists after stop, size={os.path.getsize('output.mkv')}")
        else:
            self._log("LOGS: output.mkv DOES NOT EXIST after stop")

        if proc.returncode not in (0, None):
            stderr = (proc.stderr or "").strip()
            raise WaylandPortalRecorderError(
                f"Запись завершилась с кодом {proc.returncode}: {stderr or 'неизвестная ошибка'}"
            )

    async def _run_wf_backend(self) -> tuple[bool, str | None]:
        if not _wf_backend_available():
            return False, "wf-recorder не найден, fallback Wayland невозможен"

        proc: subprocess.Popen | None = None
        started = False
        try:
            geometry = self._geometry_for_wf()
            cmd = ["wf-recorder", "-f", "output.mkv"]
            if geometry:
                cmd.extend(["-g", geometry])

            proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            with self._lock:
                self._wf_proc = proc

            await asyncio.sleep(0.2)
            if proc.poll() is not None:
                stdout, stderr = proc.communicate(timeout=1)
                details = (stderr or stdout or "").strip()
                if "wlr-screencopy" in details:
                    details = (
                        "композитор не поддерживает wlr-screencopy-unstable-v1; "
                        "используйте backend портала"
                    )
                raise WaylandPortalRecorderError(
                    f"wf-recorder не запустился (код {proc.returncode}): {details}"
                )

            with self._lock:
                self._active = True
                self._last_error = None
            started = True
            self._dispatch(lambda: self.on_start_callback(True))

            while not self._stop_event.is_set():
                if proc.poll() is not None:
                    break
                await asyncio.sleep(0.2)

            if self._stop_event.is_set():
                try:
                    proc.send_signal(signal.SIGINT)
                except Exception:
                    pass

            stdout, stderr = proc.communicate(timeout=3)
            if proc.returncode not in (0, None):
                details = (stderr or stdout or "").strip()
                raise WaylandPortalRecorderError(
                    f"wf-recorder завершился с кодом {proc.returncode}: {details or 'неизвестная ошибка'}"
                )
            self._log("Wayland запись завершена (wf-recorder)")
            return True, None
        except WaylandPortalRecorderError as exc:
            if started:
                self._notify_failure(str(exc))
                return True, str(exc)
            return False, str(exc)
        except Exception as exc:  # pragma: no cover
            message = f"Не удалось выполнить fallback запись Wayland: {exc}"
            if started:
                self._notify_failure(message)
                return True, message
            return False, message
        finally:
            with self._lock:
                was_active = self._active or started or self._wf_proc is not None
                self._active = False
                self._wf_proc = None
            if was_active and started:
                self._record_error()
                self._dispatch(lambda: self.on_start_callback(False))
    def _geometry_for_wf(self) -> str | None:
        if self._crop:
            x0, y0, x1, y1 = self._crop
            width = max(int(x1 - x0), 2)
            height = max(int(y1 - y0), 2)
            return f"{int(x0)},{int(y0)} {width}x{height}"

        if self._source_types == WINDOW_SOURCE_TYPE:
            if shutil.which("slurp") is None:
                raise WaylandPortalRecorderError("Для режима 'Программа' в fallback нужен slurp")
            selected = self._run_slurp_geometry()
            if not selected:
                raise WaylandPortalRecorderError("Выбор окна/области отменен")
            return selected

        return None

    @staticmethod
    def _run_slurp_geometry() -> str | None:
        try:
            proc = subprocess.run(
                ["slurp", "-f", "%x,%y %wx%h"],
                capture_output=True,
                text=True,
                timeout=30,
                check=False,
            )
            if proc.returncode != 0:
                return None
            raw = (proc.stdout or "").strip()
            return raw or None
        except Exception:
            return None

    @staticmethod
    def _crop_to_ffmpeg(crop: tuple[int, int, int, int] | None) -> str | None:
        if not crop:
            return None
        x0, y0, x1, y1 = crop
        width = max(int(x1 - x0), 2)
        height = max(int(y1 - y0), 2)
        x = max(int(x0), 0)
        y = max(int(y0), 0)
        if width % 2 != 0:
            width -= 1
        if height % 2 != 0:
            height -= 1
        if width < 2 or height < 2:
            return None
        return f"{width}:{height}:{x}:{y}"

    @classmethod
    def _extract_crop_from_stream(cls, stream) -> tuple[int, int, int, int] | None:
        props = cls._stream_properties(stream)
        if not props:
            return None
        candidates = [props]
        for key in ("window", "Window", "geometry", "Geometry", "rect", "Rect", "window_rect", "window-rect", "bounds", "Bounds"):
            nested = props.get(key)
            if isinstance(nested, dict):
                candidates.append(nested)
        for candidate in candidates:
            rect = cls._extract_rect_from_mapping(candidate)
            if rect:
                return rect
        return None

    def _extract_crop_from_streams(self, streams) -> None:
        if self._crop is not None:
            return
        if self._source_types != WINDOW_SOURCE_TYPE:
            return
        stream = self._pick_stream(streams, preferred_source_type=WINDOW_SOURCE_TYPE)
        crop_from_portal = self._extract_crop_from_stream(stream)

        if crop_from_portal:
            self._crop = crop_from_portal
            x0, y0, x1, y1 = crop_from_portal
            self._log(f"Расположение окна из портала: ({x0},{y0}) — ({x1},{y1})")
            return

        self._log(
            "Портал не сообщил координаты окна — запись без обрезки. "
            "На wlroots-композиторах поток уже содержит только окно."
        )

    @staticmethod
    def _try_extract_crop_from_stream_info(stream_info) -> tuple[int, int, int, int] | None:
        if not stream_info:
            return None
        return WaylandProgramScreenMode._extract_crop_from_stream(stream_info)

    @staticmethod
    def _extract_node_id(streams) -> int:
        if streams is None:
            return 0

        if hasattr(streams, "node_id"):
            try:
                return int(getattr(streams, "node_id"))
            except Exception:
                return 0

        if isinstance(streams, dict):
            if "node_id" in streams:
                try:
                    return int(streams.get("node_id", 0))
                except Exception:
                    return 0
            for value in streams.values():
                node_id = WaylandProgramScreenMode._extract_node_id(value)
                if node_id > 0:
                    return node_id
            return 0

        if isinstance(streams, (list, tuple)):
            if not streams:
                return 0
            first = streams[0]
            if hasattr(first, "node_id"):
                try:
                    return int(getattr(first, "node_id"))
                except Exception:
                    return 0
            if isinstance(first, (list, tuple)) and first:
                try:
                    return int(first[0])
                except Exception:
                    return 0
            if isinstance(first, dict):
                try:
                    return int(first.get("node_id", 0))
                except Exception:
                    return 0

        return 0

    def _log(self, message: str):
        import datetime
        import os
        timestamp = datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]
        # Пишем только в перезаписываемый каталог: в AppImage каталог бандла
        # read-only, а падение логирования не должно убивать воркер записи.
        try:
            from app_paths import get_writable_base_dir
            log_dir = os.path.join(get_writable_base_dir(), "logs")
            os.makedirs(log_dir, exist_ok=True)
            log_file = os.path.join(log_dir, "wayland.log")
            with open(log_file, "a", encoding="utf-8") as f:
                f.write(f"[{timestamp}] {message}\n")
        except Exception:
            pass
        if self.logger:
            try:
                self.logger(message)
            except Exception:
                pass

    def _notify_failure(self, message: str):
        with self._lock:
            self._last_error = message
            self._error_logged = False
        self._log(message)

    def _dispatch(self, func):
        try:
            self.root.after(0, func)
        except Exception:
            try:
                func()
            except Exception:
                pass

    def stop_recording(self):
        self._stop_event.set()
        with self._lock:
            ffmpeg_proc = self._ffmpeg_proc
            wf_proc = self._wf_proc
        if ffmpeg_proc:
            ffmpeg_proc.stop(timeout=8.0)
        if wf_proc and wf_proc.poll() is None:
            try:
                wf_proc.send_signal(signal.SIGINT)
            except Exception:
                pass
        if self._thread and self._thread.is_alive():
            self._thread.join(timeout=3.0)
        with self._lock:
            self._active = False

    def _record_error(self):
        with self._lock:
            if not self._last_error or self._error_logged:
                return
            error = self._last_error
            self._error_logged = True
        if error:
            path = write_error_report("wayland", error)
            self._log(f"Wayland: подробности ошибки ({self._selection_label}) в {path}")


class WaylandFullScreenMode(WaylandProgramScreenMode):
    def __init__(self, root, on_start_callback, fps: int = 20, logger: Any | None = None,
                 video_crf: int = 23):
        super().__init__(
            root,
            on_start_callback,
            fps=fps,
            logger=logger,
            source_types=MONITOR_SOURCE_TYPE,
            selection_label="монитор",
            video_crf=video_crf,
        )


class WaylandAreaScreenMode(WaylandProgramScreenMode):
    def __init__(
        self,
        root,
        on_start_callback,
        area: tuple[int, int, int, int],
        fps: int = 20,
        logger: Any | None = None,
        video_crf: int = 23,
    ):
        super().__init__(
            root,
            on_start_callback,
            fps=fps,
            logger=logger,
            source_types=MONITOR_SOURCE_TYPE,
            selection_label="область",
            crop=area,
            video_crf=video_crf,
        )


__all__ = [
    "WaylandProgramScreenMode",
    "WaylandFullScreenMode",
    "WaylandAreaScreenMode",
    "is_wayland_session",
    "wayland_dependency_issue",
]
