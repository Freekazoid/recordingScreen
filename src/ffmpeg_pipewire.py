"""Utilities for recording PipeWire streams via ffmpeg or GStreamer."""

from __future__ import annotations

import contextlib
import os
import shutil
import signal
import subprocess
import threading
import time
from dataclasses import dataclass
from typing import Any


@contextlib.contextmanager
def _managed_dup_fd(fd: int):
    dup_fd = os.dup(fd)
    try:
        os.set_inheritable(dup_fd, True)
        yield dup_fd
    except Exception:
        try:
            os.close(dup_fd)
        except Exception:
            pass
        raise


class FFmpegNotFoundError(RuntimeError):
    pass


class FFmpegRecordingError(RuntimeError):
    pass


class GstRecordingError(RuntimeError):
    pass


@dataclass
class RecordingOptions:
    output: str
    fps: int = 30
    width: int | None = None
    height: int | None = None
    crop: str | None = None  # format: "w:h:x:y"
    duration: int | None = None
    video_codec: str = "libx264"
    crf: int = 23
    preset: str = "veryfast"
    extra_filters: list[str] | None = None


def _resolve_ffmpeg_bin() -> str | None:
    try:
        from ffmpeg_locator import ffmpeg_command, has_ffmpeg

        if has_ffmpeg():
            return ffmpeg_command()
    except Exception:
        pass
    return shutil.which("ffmpeg")


def ffmpeg_available() -> bool:
    return bool(_resolve_ffmpeg_bin())


def pipewire_demuxer_available() -> bool:
    """Check if ffmpeg has the pipewire demuxer."""
    ffmpeg_bin = _resolve_ffmpeg_bin()
    if not ffmpeg_bin:
        return False
    try:
        result = subprocess.run(
            [ffmpeg_bin, "-hide_banner", "-demuxers"],
            capture_output=True, text=True, timeout=5,
        )
        return "pipewire" in result.stdout
    except Exception:
        return False


def gst_pipewire_available() -> bool:
    """Check if GStreamer pipewiresrc is available via Python bindings."""
    try:
        import gi
        gi.require_version("Gst", "1.0")
        from gi.repository import Gst  # noqa: F401
        return True
    except Exception:
        return False


def gst_launch_pipewire_available() -> bool:
    """System gst-launch-1.0 + pipewiresrc (works from frozen AppImage without PyGObject)."""
    if not shutil.which("gst-launch-1.0"):
        return False
    try:
        result = subprocess.run(
            ["gst-inspect-1.0", "pipewiresrc"],
            capture_output=True,
            text=True,
            timeout=5,
        )
        return result.returncode == 0
    except Exception:
        return False


def pipewire_recording_backend_available() -> bool:
    """True when we can actually encode a PipeWire screen stream after portal."""
    return (
        pipewire_demuxer_available()
        or gst_pipewire_available()
        or gst_launch_pipewire_available()
    )


def _build_filter(options: RecordingOptions) -> str | None:
    filters: list[str] = []
    if options.width and options.height:
        filters.append(f"scale={options.width}:{options.height}")
    if options.crop:
        filters.append(f"crop={options.crop}")
    if options.extra_filters:
        filters.extend(options.extra_filters)
    if not filters:
        return None
    return ",".join(filters)


def _build_command(ffmpeg_bin: str, node_id: int, options: RecordingOptions) -> list[str]:
    cmd: list[str] = [
        ffmpeg_bin,
        "-loglevel",
        "error",
        "-y",
        "-f",
        "pipewire",
        "-i",
        str(node_id),
        "-c:v",
        options.video_codec,
        "-preset",
        options.preset,
        "-crf",
        str(options.crf),
    ]

    filter_chain = _build_filter(options)
    if filter_chain:
        cmd.extend(["-vf", filter_chain])

    if options.fps:
        cmd.extend(["-r", str(options.fps)])

    if options.duration:
        cmd.extend(["-t", str(options.duration)])

    cmd.append(options.output)
    return cmd


class FFmpegPipewireProcess:
    def __init__(self, node_id: int, fd: int, options: RecordingOptions):
        ffmpeg_bin = _resolve_ffmpeg_bin()
        if not ffmpeg_bin:
            raise FFmpegNotFoundError(
                "ffmpeg not found (bundled bin/ or system PATH)"
            )

        self._fd = -1
        self._env = os.environ.copy()
        with _managed_dup_fd(fd) as dup_fd:
            self._fd = dup_fd
        self._cmd = _build_command(ffmpeg_bin, node_id, options)
        self._env["PIPEWIRE_REMOTE"] = f"unix:fd={self._fd}"

        self._proc: subprocess.Popen | None = None
        self._stderr = ""
        self._stdout = ""
        self._monitor_thread: threading.Thread | None = None
        self._finished = threading.Event()

    def start(self):
        if self._proc is not None:
            return
        self._proc = subprocess.Popen(
            self._cmd,
            env=self._env,
            pass_fds=(self._fd,) if self._fd >= 0 else (),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        def _monitor():
            try:
                stdout, stderr = self._proc.communicate()
                self._stdout = stdout or ""
                self._stderr = stderr or ""
            finally:
                if self._fd >= 0:
                    try:
                        os.close(self._fd)
                    except Exception:
                        pass
                    self._fd = -1
                self._finished.set()

        self._monitor_thread = threading.Thread(target=_monitor, daemon=True)
        self._monitor_thread.start()

    def stop(self, timeout: float = 3.0):
        if self._proc is None or self._finished.is_set():
            return

        try:
            self._proc.send_signal(signal.SIGINT)
        except Exception:
            pass

        if not self._finished.wait(timeout):
            try:
                self._proc.terminate()
            except Exception:
                pass

        if not self._finished.wait(timeout):
            try:
                self._proc.kill()
            except Exception:
                pass

        self._finished.wait(timeout)

    def wait(self, timeout: float | None = None) -> bool:
        return self._finished.wait(timeout)

    @property
    def returncode(self) -> int | None:
        if self._proc is None:
            return None
        return self._proc.returncode

    @property
    def stderr(self) -> str:
        return self._stderr

    @property
    def stdout(self) -> str:
        return self._stdout


class GstPipewireProcess:
    """GStreamer-based PipeWire recorder (fallback when ffmpeg lacks pipewire demuxer)."""

    def __init__(self, node_id: int, fd: int, options: RecordingOptions, log_callback=None):
        self._log_callback = log_callback or (lambda msg: None)

        self._log("GstPipewireProcess: init start")
        try:
            import gi
            gi.require_version("Gst", "1.0")
            from gi.repository import Gst
            Gst.init(None)
            self._Gst = Gst
        except Exception as e:
            raise GstRecordingError(
                f"GStreamer bindings not available: {e}"
            )

        self._fd = -1
        with _managed_dup_fd(fd) as dup_fd:
            self._fd = dup_fd
        self._options = options
        self._node_id = node_id
        self._pipeline: Any | None = None
        self._bus: Any | None = None
        self._src: Any | None = None
        self._finished = threading.Event()
        self._returncode: int | None = None
        self._error_message = ""
        self._lock = threading.Lock()
        self._crop_element = None
        self._crop_info = None
        self._capsfilter = None
        self._log(f"GstPipewireProcess: output={options.output}, fd={self._fd}, node_id={node_id}")

    def _log(self, msg):
        self._log_callback(msg)

    def _set_prop(self, element, name: str, value) -> None:
        try:
            element.set_property(name, value)
        except Exception as exc:
            self._log(f"GstPipewireProcess: property {name}={value!r} skipped: {exc}")

    def _build_pipeline(self):
        gst = self._Gst
        pipeline = gst.Pipeline.new("pipewire-recorder")
        self._log("GstPipewireProcess: building pipeline")

        src = gst.ElementFactory.make("pipewiresrc", "source")
        if not src:
            raise GstRecordingError("Failed to create pipewiresrc element")
        self._set_prop(src, "fd", self._fd)
        self._set_prop(src, "path", str(self._node_id))
        self._set_prop(src, "always-copy", True)
        self._set_prop(src, "do-timestamp", True)
        self._src = src
        self._log("GstPipewireProcess: pipewiresrc created")

        queue = gst.ElementFactory.make("queue", "queue")
        if not queue:
            raise GstRecordingError("Failed to create queue element")
        self._set_prop(queue, "max-size-buffers", 0)
        self._set_prop(queue, "max-size-bytes", 0)
        self._set_prop(queue, "max-size-time", 0)

        convert = gst.ElementFactory.make("videoconvert", "convert")
        if not convert:
            raise GstRecordingError("Failed to create videoconvert element")
        self._set_prop(convert, "n-threads", 0)

        capsfilter = gst.ElementFactory.make("capsfilter", "caps")
        if not capsfilter:
            raise GstRecordingError("Failed to create capsfilter element")
        # NV12 is friendlier for x264enc live encode than forcing I420 too early.
        caps = gst.Caps.from_string("video/x-raw,format=NV12")
        capsfilter.set_property("caps", caps)
        self._capsfilter = capsfilter

        self._crop_element = None
        self._crop_info = None
        if self._options.crop:
            try:
                parts = self._options.crop.split(":")
                cw, ch, cx, cy = [int(p) for p in parts]
                if cw > 5 and ch > 5:
                    crop_el = gst.ElementFactory.make("videocrop", "crop")
                    if crop_el:
                        self._crop_element = crop_el
                        self._crop_info = (cx, cy, cw, ch)
            except (ValueError, IndexError):
                pass

        scale = gst.ElementFactory.make("videoscale", "scale")
        if not scale:
            raise GstRecordingError("Failed to create videoscale element")
        self._set_prop(scale, "method", 1)  # bilinear

        rate = gst.ElementFactory.make("videorate", "rate")
        if not rate:
            raise GstRecordingError("Failed to create videorate element")
        fps = max(1, int(self._options.fps or 30))
        rate_caps = gst.ElementFactory.make("capsfilter", "rate_caps")
        if not rate_caps:
            raise GstRecordingError("Failed to create fps capsfilter")
        rate_caps.set_property(
            "caps",
            gst.Caps.from_string(f"video/x-raw,framerate={fps}/1"),
        )

        enc = gst.ElementFactory.make("x264enc", "encoder")
        if not enc:
            raise GstRecordingError("Failed to create x264enc element")
        self._set_prop(enc, "tune", 0x4)  # zerolatency
        self._set_prop(enc, "speed-preset", 3)  # veryfast
        self._set_prop(enc, "byte-stream", False)
        self._set_prop(enc, "key-int-max", fps * 2)
        self._set_prop(enc, "threads", 0)
        # Constant quality (closer to ffmpeg CRF than CBR for live capture).
        crf = max(1, min(50, self._options.crf or 23))
        self._set_prop(enc, "pass", 5)  # qual
        self._set_prop(enc, "quantizer", crf)

        parse = gst.ElementFactory.make("h264parse", "parse")
        if not parse:
            raise GstRecordingError("Failed to create h264parse element")

        # output.mkv must use matroska — qtmux produced a mislabeled/corrupt QT file.
        muxer = gst.ElementFactory.make("matroskamux", "muxer")
        if not muxer:
            raise GstRecordingError("Failed to create matroskamux element")
        self._set_prop(muxer, "streamable", True)
        self._set_prop(muxer, "writing-app", "recordingscreen")

        sink = gst.ElementFactory.make("filesink", "sink")
        if not sink:
            raise GstRecordingError("Failed to create filesink element")
        self._set_prop(sink, "location", self._options.output)
        self._set_prop(sink, "sync", False)
        self._set_prop(sink, "async", False)
        self._log(f"GstPipewireProcess: filesink output={self._options.output}")

        elements = [src, queue, convert, capsfilter]
        if self._crop_element:
            elements.append(self._crop_element)
        elements.extend([scale, rate, rate_caps, enc, parse, muxer, sink])

        for element in elements:
            pipeline.add(element)

        for left, right in zip(elements, elements[1:]):
            if not left.link(right):
                raise GstRecordingError(
                    f"Failed to link pipeline elements: {left.get_name()} -> {right.get_name()}"
                )

        self._pipeline = pipeline
        self._bus = pipeline.get_bus()
        self._log("GstPipewireProcess: pipeline built and linked")

    def start(self):
        if self._pipeline is not None:
            self._log("GstPipewireProcess: start called but already running")
            return
        self._build_pipeline()
        self._add_timestamp_probe()
        if self._crop_element:
            self._add_crop_probe()

        ret = self._pipeline.set_state(self._Gst.State.PLAYING)
        ret_names = {0: "FAILURE", 1: "SUCCESS", 2: "ASYNC", 3: "NO_PREROLL"}
        self._log(f"GstPipewireProcess: set_state(PLAYING) -> {ret_names.get(ret, str(ret))}")
        if ret == self._Gst.StateChangeReturn.FAILURE:
            raise GstRecordingError("GStreamer pipeline failed to enter PLAYING")

        import os
        file_exists = os.path.exists(self._options.output)
        self._log(f"GstPipewireProcess: output file exists after start: {file_exists}")

        def _bus_watch():
            bus = self._bus
            if bus is None:
                self._finished.set()
                return
            warning_count = 0
            try:
                while not self._finished.is_set():
                    msg = bus.timed_pop_filtered(
                        200 * 1000 * 1000,
                        self._Gst.MessageType.ERROR
                        | self._Gst.MessageType.EOS
                        | self._Gst.MessageType.WARNING
                        | self._Gst.MessageType.INFO,
                    )
                    if msg is None:
                        continue
                    t = msg.type
                    if t == self._Gst.MessageType.WARNING:
                        warning_count += 1
                        # PipeWire/videoconvert can spam identical warnings; keep log readable.
                        if warning_count <= 5 or warning_count % 50 == 0:
                            dbg = msg.parse_warning()
                            self._log(
                                f"GstPipewireProcess WARNING ({warning_count}): {dbg}"
                            )
                    elif t == self._Gst.MessageType.INFO:
                        dbg = msg.parse_info()
                        self._log(f"GstPipewireProcess INFO: {dbg}")
                    elif t == self._Gst.MessageType.ERROR:
                        err, dbg = msg.parse_error()
                        self._error_message = f"{err.message} ({dbg})"
                        self._log(f"GstPipewireProcess ERROR: {self._error_message}")
                        self._returncode = 1
                        self._finished.set()
                        break
                    elif t == self._Gst.MessageType.EOS:
                        self._log("GstPipewireProcess: EOS received")
                        self._returncode = 0
                        self._finished.set()
                        break
            finally:
                self._cleanup()
                self._log("GstPipewireProcess: bus watcher stopped")

        self._monitor_thread = threading.Thread(target=_bus_watch, daemon=True)
        self._monitor_thread.start()

    def _shutdown(self):
        with self._lock:
            if self._pipeline is not None:
                self._log("GstPipewireProcess: setting pipeline to NULL")
                self._pipeline.set_state(self._Gst.State.NULL)
                self._pipeline = None
                self._bus = None
                self._src = None
                self._crop_element = None
                self._capsfilter = None
                self._log("GstPipewireProcess: pipeline stopped")

    def _cleanup(self):
        if self._fd >= 0:
            with self._lock:
                if self._fd >= 0:
                    try:
                        os.close(self._fd)
                    except Exception:
                        pass
                    self._fd = -1

    def _add_timestamp_probe(self):
        src_pad = self._src.get_static_pad("src")
        fps = self._options.fps or 30
        start_time = [None]

        def _probe(pad, info):
            buf = info.get_buffer()
            if buf:
                now = time.monotonic()
                if start_time[0] is None:
                    start_time[0] = now
                elapsed_ns = int((now - start_time[0]) * self._Gst.SECOND)
                buf.pts = elapsed_ns
                buf.duration = int(self._Gst.SECOND / fps)
            return self._Gst.PadProbeReturn.OK

        src_pad.add_probe(self._Gst.PadProbeType.BUFFER, _probe)

    def _add_crop_probe(self):
        if not self._crop_info or not self._crop_element or not self._capsfilter:
            return
        cx, cy, cw, ch = self._crop_info
        gst = self._Gst
        src_pad = self._capsfilter.get_static_pad("src")

        def _crop_probe(pad, info):
            event = info.get_event()
            if event is None or event.type != gst.EventType.CAPS:
                return gst.PadProbeReturn.OK
            caps = event.parse_caps()
            if caps is None:
                return gst.PadProbeReturn.OK
            structure = caps.get_structure(0)
            if structure is None:
                return gst.PadProbeReturn.OK
            ok_w, src_w = structure.get_int("width")
            ok_h, src_h = structure.get_int("height")
            if ok_w and ok_h and src_w > 0 and src_h > 0:
                crop_w = cw if cw % 2 == 0 else cw - 1
                crop_h = ch if ch % 2 == 0 else ch - 1
                right = max(0, src_w - (cx + crop_w))
                bottom = max(0, src_h - (cy + crop_h))
                self._crop_element.set_property("left", cx)
                self._crop_element.set_property("right", right)
                self._crop_element.set_property("top", cy)
                self._crop_element.set_property("bottom", bottom)
                return gst.PadProbeReturn.REMOVE
            return gst.PadProbeReturn.OK

        src_pad.add_probe(gst.PadProbeType.EVENT_DOWNSTREAM, _crop_probe)

    def stop(self, timeout: float = 8.0):
        if self._finished.is_set():
            self._log("GstPipewireProcess: stop called but already finished")
            return
        self._log(f"GstPipewireProcess: stopping (timeout={timeout}s)")
        import os
        before_exists = os.path.exists(self._options.output)
        self._log(f"GstPipewireProcess: output file exists before stop: {before_exists}")

        if self._pipeline is not None:
            self._pipeline.send_event(self._Gst.Event.new_eos())
            self._log("GstPipewireProcess: EOS event sent")
        if self._finished.wait(timeout):
            self._log("GstPipewireProcess: finished via EOS/ERROR")
            self._shutdown()
            after_exists = os.path.exists(self._options.output)
            after_size = os.path.getsize(self._options.output) if after_exists else 0
            self._log(f"GstPipewireProcess: output file exists after stop: {after_exists}, size={after_size}")
            return
        self._log("GstPipewireProcess: EOS timeout, force-stopping")
        self._shutdown()
        self._finished.set()

        after_exists = os.path.exists(self._options.output)
        after_size = os.path.getsize(self._options.output) if after_exists else 0
        self._log(f"GstPipewireProcess: output file exists after stop: {after_exists}, size={after_size}")

    def close(self):
        self._shutdown()
        self._finished.set()
        self._cleanup()

    def wait(self, timeout: float | None = None) -> bool:
        return self._finished.wait(timeout)

    @property
    def returncode(self) -> int | None:
        return self._returncode

    @property
    def stderr(self) -> str:
        return self._error_message

    @property
    def stdout(self) -> str:
        return ""


class GstLaunchPipewireProcess:
    """Record PipeWire via system gst-launch-1.0 (no Python GStreamer bindings)."""

    def __init__(self, node_id: int, fd: int, options: RecordingOptions, log_callback=None):
        self._log = log_callback or (lambda _msg: None)
        launch = shutil.which("gst-launch-1.0")
        if not launch:
            raise GstRecordingError("gst-launch-1.0 not found")

        self._fd = -1
        with _managed_dup_fd(fd) as dup_fd:
            self._fd = dup_fd

        fps = max(1, int(options.fps or 30))
        crf = max(1, min(50, int(options.crf or 23)))
        # Keep pipeline close to GstPipewireProcess; window streams are usually pre-cropped.
        pipeline = (
            f"pipewiresrc fd={self._fd} path={int(node_id)} always-copy=true do-timestamp=true ! "
            f"queue max-size-buffers=0 max-size-bytes=0 max-size-time=0 ! "
            f"videoconvert ! video/x-raw,format=NV12 ! "
            f"videoscale method=1 ! videorate ! video/x-raw,framerate={fps}/1 ! "
            f"x264enc tune=zerolatency speed-preset=veryfast pass=qual quantizer={crf} "
            f"key-int-max={fps * 2} ! "
            f"h264parse ! matroskamux streamable=true ! "
            f"filesink location={options.output} sync=false async=false"
        )
        self._cmd = [launch, "-e", "-q"] + pipeline.split()
        self._log(f"GstLaunchPipewireProcess: cmd={' '.join(self._cmd)}")

        self._proc: subprocess.Popen | None = None
        self._stderr = ""
        self._stdout = ""
        self._monitor_thread: threading.Thread | None = None
        self._finished = threading.Event()

    def start(self):
        if self._proc is not None:
            return
        self._proc = subprocess.Popen(
            self._cmd,
            pass_fds=(self._fd,) if self._fd >= 0 else (),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )

        def _monitor():
            try:
                stdout, stderr = self._proc.communicate()
                self._stdout = stdout or ""
                self._stderr = stderr or ""
            finally:
                if self._fd >= 0:
                    try:
                        os.close(self._fd)
                    except Exception:
                        pass
                    self._fd = -1
                self._finished.set()

        self._monitor_thread = threading.Thread(target=_monitor, daemon=True)
        self._monitor_thread.start()

    def stop(self, timeout: float = 5.0):
        if self._proc is None or self._finished.is_set():
            return
        try:
            self._proc.send_signal(signal.SIGINT)
        except Exception:
            pass
        if not self._finished.wait(timeout):
            try:
                self._proc.terminate()
            except Exception:
                pass
        if not self._finished.wait(timeout):
            try:
                self._proc.kill()
            except Exception:
                pass
        self._finished.wait(timeout)

    def wait(self, timeout: float | None = None) -> bool:
        return self._finished.wait(timeout)

    @property
    def returncode(self) -> int | None:
        if self._proc is None:
            return None
        return self._proc.returncode

    @property
    def stderr(self) -> str:
        return self._stderr

    @property
    def stdout(self) -> str:
        return self._stdout


def record_pipewire_stream(node_id: int, fd: int, options: RecordingOptions) -> None:
    if pipewire_demuxer_available():
        proc = FFmpegPipewireProcess(node_id=node_id, fd=fd, options=options)
        proc.start()
        proc.wait()
        if proc.returncode != 0:
            raise FFmpegRecordingError(
                f"ffmpeg exited with code {proc.returncode}.\n"
                f"Stderr: {proc.stderr}"
            )
        return

    if gst_pipewire_available():
        proc = GstPipewireProcess(node_id=node_id, fd=fd, options=options)  # type: ignore[assignment]
        proc.start()
        proc.wait()
        if proc.returncode != 0:
            raise GstRecordingError(
                f"GStreamer recording failed:\n{proc.stderr}"
            )
        return

    if gst_launch_pipewire_available():
        proc = GstLaunchPipewireProcess(node_id=node_id, fd=fd, options=options)  # type: ignore[assignment]
        proc.start()
        proc.wait()
        if proc.returncode != 0:
            raise GstRecordingError(
                f"gst-launch recording failed:\n{proc.stderr}"
            )
        return

    raise FFmpegNotFoundError(
        "No available PipeWire recording backend. "
        "Install gstreamer1.0-pipewire (gst-launch-1.0) or ffmpeg with pipewire demuxer."
    )


__all__ = [
    "record_pipewire_stream",
    "RecordingOptions",
    "FFmpegPipewireProcess",
    "GstPipewireProcess",
    "GstLaunchPipewireProcess",
    "FFmpegNotFoundError",
    "FFmpegRecordingError",
    "GstRecordingError",
    "ffmpeg_available",
    "pipewire_demuxer_available",
    "gst_pipewire_available",
    "gst_launch_pipewire_available",
    "pipewire_recording_backend_available",
]
