
import cv2

# Avoid hardware / unavailable codecs that break VideoWriter on Linux/Wayland
_FOURCC_PREFERENCE = (
    "mp4v",
    "XVID",
    "MJPG",
)


def sanitize_frame_size(width: int, height: int) -> tuple[int, int]:
    w = max(int(width), 2)
    h = max(int(height), 2)
    if w % 2:
        w -= 1
    if h % 2:
        h -= 1
    return w, h


def create_video_writer(filename: str, fps: float, size: tuple[int, int]) -> tuple[cv2.VideoWriter, tuple[int, int], str]:
    w, h = sanitize_frame_size(*size)
    frame_size = (w, h)
    last_error = None

    for code in _FOURCC_PREFERENCE:
        try:
            fourcc = cv2.VideoWriter_fourcc(*code)  # type: ignore[attr-defined]
            writer = cv2.VideoWriter(filename, fourcc, max(float(fps), 1.0), frame_size)
            if writer.isOpened():
                return writer, frame_size, code
            writer.release()
        except Exception as exc:  # noqa: BLE001 - сохранение последней ошибки для отладки
            last_error = exc

    if last_error:
        raise RuntimeError(f"Не удалось создать VideoWriter ({filename}): {last_error}")
    raise RuntimeError(f"Не удалось создать VideoWriter ({filename})")
