"""PipeWire-based recorder for Wayland screen capture via GStreamer."""

from __future__ import annotations

import os
import threading
from collections.abc import Callable

_GST_AVAILABLE = False
_GST_ERROR_MESSAGE = ""

try:
    import gi

    gi.require_version("Gst", "1.0")
    gi.require_version("GLib", "2.0")
    from gi.repository import GLib, Gst

    Gst.init(None)
    _GST_AVAILABLE = True
except Exception as exc:  # pragma: no cover - depends on system packages
    Gst = GLib = None
    _GST_ERROR_MESSAGE = str(exc)


class WaylandPipewireError(RuntimeError):
    pass


def gstreamer_available() -> bool:
    return _GST_AVAILABLE


def gstreamer_dependency_error() -> str:
    return _GST_ERROR_MESSAGE or "GStreamer (pipewiresrc) bindings unavailable"


class WaylandPipewireRecorder:
    def __init__(
        self,
        pipewire_fd: int,
        filename: str,
        fps: int = 20,
        bitrate: int = 8000,
        on_error: Callable[[str], None] | None = None,
        on_finished: Callable[[bool], None] | None = None,
    ):
        if not gstreamer_available():
            raise WaylandPipewireError(
                "GStreamer недоступен. Установите PyGObject и gstreamer1.0-plugins-* пакеты."
            )

        self._fd = os.dup(pipewire_fd)
        self._filename = filename
        self._fps = max(int(fps), 1)
        self._bitrate = max(int(bitrate), 1000)
        self._on_error = on_error
        self._on_finished = on_finished

        self._pipeline = None
        self._loop: GLib.MainLoop | None = None
        self._thread: threading.Thread | None = None
        self._finished_event = threading.Event()
        self._success = False
        self._error_reported = False

    def start(self):
        if self._pipeline is not None:
            return

        if not self._pipeline_supports_pipewiresrc():
            raise WaylandPipewireError(
                "Плагин GStreamer pipewiresrc не найден. Установите gstreamer1.0-pipewire."
            )

        self._error_reported = False
        pipeline_desc = (
            f"pipewiresrc fd={self._fd} do-timestamp=true ! queue max-size-buffers=20 ! "
            f"videoconvert ! video/x-raw,framerate={self._fps}/1 ! "
            f"x264enc tune=zerolatency speed-preset=superfast bitrate={self._bitrate} key-int-max={self._fps * 2} ! "
            f"queue ! matroskamux name=mux ! filesink name=sink async=false"
        )

        try:
            self._pipeline = Gst.parse_launch(pipeline_desc)
        except Exception as exc:  # pragma: no cover
            os.close(self._fd)
            self._fd = -1
            raise WaylandPipewireError(f"Не удалось создать конвейер GStreamer: {exc}") from exc

        sink = self._pipeline.get_by_name("sink")
        if sink is None:
            os.close(self._fd)
            self._fd = -1
            raise WaylandPipewireError("Не удалось инициализировать filesink для записи")
        sink.set_property("location", self._filename)

        bus = None
        try:
            bus = self._pipeline.get_bus()
            bus.add_signal_watch()
            bus.connect("message", self._on_bus_message)

            self._loop = GLib.MainLoop()
        except Exception as exc:  # pragma: no cover
            try:
                if bus is not None:
                    bus.remove_signal_watch()
            except Exception:
                pass
            try:
                self._pipeline.set_state(Gst.State.NULL)
            except Exception:
                pass
            if self._fd >= 0:
                os.close(self._fd)
                self._fd = -1
            raise WaylandPipewireError(f"Не удалось подготовить конвейер GStreamer: {exc}") from exc

        def _run_loop():
            try:
                self._pipeline.set_state(Gst.State.PLAYING)
                self._loop.run()
                if not self._error_reported:
                    self._success = True
            except Exception as exc:
                self._success = False
                self._report_error(f"Ошибка GStreamer: {exc}")
            finally:
                try:
                    bus = self._pipeline.get_bus()
                    bus.remove_signal_watch()
                except Exception:
                    pass
                try:
                    self._pipeline.set_state(Gst.State.NULL)
                except Exception:
                    pass
                if self._fd >= 0:
                    os.close(self._fd)
                    self._fd = -1
                self._finished_event.set()
                if self._on_finished:
                    try:
                        self._on_finished(self._success)
                    except Exception:
                        pass

        self._thread = threading.Thread(target=_run_loop, daemon=True)
        self._thread.start()

    def stop(self, timeout: float = 2.0):
        if self._loop:
            self._loop.quit()
        if self._thread:
            self._thread.join(timeout=timeout)
        if self._pipeline is not None:
            try:
                bus = self._pipeline.get_bus()
                bus.remove_signal_watch()
            except Exception:
                pass
        self._pipeline = None
        self._thread = None
        self._loop = None

    def wait(self, timeout: float | None = None) -> bool:
        return self._finished_event.wait(timeout)

    @property
    def success(self) -> bool:
        return self._success

    def _on_bus_message(self, _bus, message):
        msg_type = message.type
        if msg_type == Gst.MessageType.ERROR:
            err, debug = message.parse_error()
            self._success = False
            self._error_reported = True
            description = err.message
            if debug:
                description = f"{description} ({debug})"
            self._report_error(description)
            if self._loop:
                self._loop.quit()
        elif msg_type == Gst.MessageType.EOS:
            self._error_reported = False
            if self._loop:
                self._loop.quit()

    def _report_error(self, message: str):
        if self._on_error:
            try:
                self._on_error(message)
            except Exception:
                pass

    @staticmethod
    def _pipeline_supports_pipewiresrc() -> bool:
        if not gstreamer_available():
            return False
        registry = Gst.Registry.get()
        return registry.find_plugin("pipewire") is not None


__all__ = [
    "WaylandPipewireRecorder",
    "WaylandPipewireError",
    "gstreamer_available",
    "gstreamer_dependency_error",
]
