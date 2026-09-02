import datetime
import os
import re
import subprocess

from app_paths import safe_timestamp
from ffmpeg_locator import ffmpeg_command, ffprobe_command


def _clamp(value, min_value, max_value):
    """Ограничивает value диапазоном [min_value, max_value]."""
    return max(min_value, min(max_value, value))


def _build_video_args(output_format, video_crf):
    """Собирает аргументы ffmpeg для кодека видео по формату и уровню CRF."""
    fmt = (output_format or "mp4").lower().strip()
    crf = _clamp(int(video_crf), 0, 51)

    if fmt == "webm":
        vp9_crf = _clamp(int(round((crf / 51.0) * 63)), 0, 63)
        return ["-c:v", "libvpx-vp9", "-crf", str(vp9_crf), "-b:v", "0", "-deadline", "good"]

    if fmt == "avi":
        q_value = _clamp(int(round((crf / 51.0) * 30)), 2, 31)
        return ["-c:v", "mpeg4", "-q:v", str(q_value)]

    return ["-c:v", "libx264", "-preset", "medium", "-crf", str(crf), "-pix_fmt", "yuv420p"]


def _build_audio_args(output_format, audio_mode):
    """Собирает аргументы ffmpeg для аудио по формату и режиму."""
    fmt = (output_format or "mp4").lower().strip()
    mode = (audio_mode or "copy").lower().strip()

    if mode == "none":
        return ["-an"]

    if mode == "copy":
        if fmt == "webm":
            return ["-c:a", "libopus", "-b:a", "128k"]
        if fmt == "mp4":
            return ["-c:a", "aac", "-b:a", "128k"]
        return ["-c:a", "copy"]

    if fmt == "webm":
        return ["-c:a", "libopus", "-b:a", "128k"]
    return ["-c:a", "aac", "-b:a", "128k"]


def _run_merge(command):
    """Запускает команду слияния и возвращает True при успехе."""
    try:
        subprocess.run(command, check=True, capture_output=True)
        return True
    except subprocess.CalledProcessError as e:
        print(f"MERGE ERROR (rc={e.returncode}): {e.stderr.decode(errors='replace')[-500:]}")
        return False
    except Exception as e:
        print(f"MERGE EXCEPTION: {type(e).__name__}: {e}")
        return False


def _probe_video_size(video_path: str) -> tuple[int, int] | None:
    """Возвращает размеры видео (ширина, высота) через ffprobe или None."""
    ffprobe_bin = ffprobe_command()
    if not os.path.exists(video_path):
        return None
    try:
        proc = subprocess.run(
            [
                ffprobe_bin,
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height",
                "-of",
                "csv=s=x:p=0",
                video_path,
            ],
            capture_output=True,
            text=True,
            timeout=8,
            check=False,
        )
        raw = (proc.stdout or "").strip()
        if not raw:
            return None
        parts = raw.split("x")
        if len(parts) != 2:
            return None
        return int(parts[0]), int(parts[1])
    except Exception:
        return None


def detect_crop(video_path: str, sample_seconds: float = 2.0) -> str | None:
    """Определяет необходимость обрезки видео через фильтр cropdetect."""
    ffmpeg_bin = ffmpeg_command()
    if not os.path.exists(video_path):
        return None

    sample_seconds = max(float(sample_seconds), 0.5)
    cmd = [
        ffmpeg_bin,
        "-hide_banner",
        "-nostdin",
        "-loglevel",
        "info",
        "-i",
        video_path,
        "-vf",
        "cropdetect=24:16:0",
        "-t",
        f"{sample_seconds:.2f}",
        "-an",
        "-sn",
        "-f",
        "null",
        "-",
    ]

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=30, check=False)
    except Exception:
        return None

    output = (proc.stderr or "") + "\n" + (proc.stdout or "")
    matches = re.findall(r"crop=([0-9]+:[0-9]+:[0-9]+:[0-9]+)", output)
    if not matches:
        return None

    crop = matches[-1]
    try:
        w, h, x, y = [int(part) for part in crop.split(":")]
    except Exception:
        return None
    if w < 2 or h < 2:
        return None

    full = _probe_video_size(video_path)
    if full and w == full[0] and h == full[1] and x == 0 and y == 0:
        return None

    return crop


def _probe_duration_seconds(path: str) -> float | None:
    """Возвращает длительность видео в секундах через ffprobe или None."""
    ffprobe_bin = ffprobe_command()
    if not path or not os.path.exists(path):
        return None
    try:
        out = subprocess.check_output(
            [
                ffprobe_bin,
                "-v", "error",
                "-show_entries", "format=duration",
                "-of", "default=noprint_wrappers=1:nokey=1",
                path,
            ],
            stderr=subprocess.DEVNULL,
            timeout=15,
        ).decode().strip()
        if not out or out.upper() == "N/A":
            return None
        value = float(out)
        return value if value > 0 else None
    except Exception:
        return None


def _remux_video(video_path: str) -> str | None:
    """Пересобирает контейнер с пересчитанными таймкодами; возвращает путь или None."""
    ffmpeg_bin = ffmpeg_command()
    repaired = video_path + ".repaired.mkv"
    try:
        subprocess.run(
            [
                ffmpeg_bin, "-y",
                "-fflags", "+genpts+igndts",
                "-i", video_path,
                "-c", "copy",
                repaired,
            ],
            capture_output=True,
            timeout=120,
            check=False,
        )
        if os.path.exists(repaired) and os.path.getsize(repaired) > 1024:
            duration = _probe_duration_seconds(repaired)
            if duration and duration >= 0.5:
                return repaired
        try:
            os.remove(repaired)
        except Exception:
            pass
    except Exception:
        try:
            os.remove(repaired)
        except Exception:
            pass
    return None


def merge_av(video_path, audio_path, output_path=None, audio_offset=None, output_format="mp4", video_crf=23, audio_mode="copy", video_filter=None):
    """Объединяет видео и аудио в один файл через ffmpeg."""
    ffmpeg_bin = ffmpeg_command()

    if not os.path.exists(video_path):
        return None

    if audio_mode != "none":
        if not os.path.exists(audio_path):
            return None
        if os.path.getsize(audio_path) < 1000:
            return None

    if output_path is None:
        ext = (output_format or "mp4").lower().strip()
        output_path = f"{safe_timestamp()}.{ext}"

    source_video = video_path
    duration = _probe_duration_seconds(video_path)
    if duration is None or duration < 0.5:
        remuxed = _remux_video(video_path)
        if remuxed:
            print(f"MERGE: remuxed broken container -> {remuxed}")
            source_video = remuxed
            duration = _probe_duration_seconds(remuxed)

    command = [ffmpeg_bin, "-y", "-fflags", "+genpts"]
    command.extend(["-i", source_video])

    if audio_mode != "none":
        if audio_offset and audio_offset > 0.03:
            command.extend(["-ss", f"{audio_offset:.3f}"])
        command.extend(["-i", audio_path, "-map", "0:v:0", "-map", "1:a:0"])

    command.extend(_build_video_args(output_format, video_crf))
    if video_filter:
        command.extend(["-vf", video_filter])
    audio_args = _build_audio_args(output_format, audio_mode)
    command.extend(audio_args)
    if audio_args not in (["-an"], ["-c:a", "copy"]):
        command.extend(["-af", "loudnorm=I=-16:TP=-1.5:LRA=11"])

    # -shortest используем только когда видео достаточно длинное, иначе аудио
    # обрезалось бы до некорректной почти нулевой длительности.
    if audio_mode != "none" and duration and duration >= 0.5:
        command.append("-shortest")

    command.append(output_path)

    try:
        ok = _run_merge(command)
        if ok:
            return output_path

        if (audio_mode or "").lower().strip() == "copy":
            fallback_command = [ffmpeg_bin, "-y", "-fflags", "+genpts"]
            fallback_command.extend(["-i", source_video])
            if audio_offset and audio_offset > 0.03:
                fallback_command.extend(["-ss", f"{audio_offset:.3f}"])
            fallback_command.extend(["-i", audio_path, "-map", "0:v:0", "-map", "1:a:0"])
            fallback_command.extend(_build_video_args(output_format, video_crf))
            if video_filter:
                fallback_command.extend(["-vf", video_filter])
            fb_audio_args = _build_audio_args(output_format, "aac128")
            fallback_command.extend(fb_audio_args)
            if fb_audio_args not in (["-an"], ["-c:a", "copy"]):
                fallback_command.extend(["-af", "loudnorm=I=-16:TP=-1.5:LRA=11"])
            if duration and duration >= 0.5:
                fallback_command.append("-shortest")
            fallback_command.append(output_path)

            if _run_merge(fallback_command):
                return output_path

        return None
    finally:
        if source_video != video_path and os.path.exists(source_video):
            try:
                os.remove(source_video)
            except Exception:
                pass
