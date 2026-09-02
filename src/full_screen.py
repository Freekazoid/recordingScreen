import threading
import time

import cv2
import mss
import numpy as np

from video_writer_utils import create_video_writer


class FullScreenMode:
    """Режим записи всего экрана через mss в отдельном потоке."""

    def __init__(self, fps=20):
        """Сохраняет FPS записи и начальное состояние (запись выключена)."""
        self.is_recording = False
        self.fps = fps
        self.thread = None

    def start_recording(self, duration_seconds=None):
        """Запускает запись всего экрана в фоновом потоке."""
        self.is_recording = True
        self.thread = threading.Thread(target=self._record, args=(duration_seconds,), daemon=True)
        self.thread.start()

    def stop_recording(self):
        """Останавливает запись, дождавшись завершения фонового потока."""
        self.is_recording = False
        if self.thread and self.thread.is_alive():
            self.thread.join(timeout=2.0)

    def _record(self, duration_seconds=None):
        """Тело фонового потока: снимает монитор через mss и пишет кадры в видеофайл."""
        filename = "output.mkv"
        with mss.mss() as sct:
            monitor = sct.monitors[0]
            width = monitor["width"]
            height = monitor["height"]

            try:
                out, (target_w, target_h), codec = create_video_writer(filename, self.fps, (width, height))
                print(f"[FullScreenMode] Используется кодек {codec} для размеров {target_w}x{target_h}")
            except Exception as exc:
                print(f"[FullScreenMode] Ошибка инициализации VideoWriter: {exc}")
                self.is_recording = False
                return

            frame_interval = 1.0 / max(self.fps, 1)
            next_frame_time = time.perf_counter()

            while self.is_recording:
                now = time.perf_counter()
                if now < next_frame_time:
                    time.sleep(next_frame_time - now)

                img = np.array(sct.grab(monitor))
                frame = cv2.cvtColor(img, cv2.COLOR_BGRA2BGR)
                if frame.shape[1] != target_w or frame.shape[0] != target_h:
                    frame = cv2.resize(frame, (target_w, target_h), interpolation=cv2.INTER_AREA)
                out.write(frame)

                next_frame_time += frame_interval
                if time.perf_counter() - next_frame_time > frame_interval:
                    next_frame_time = time.perf_counter()

            out.release()
            print("[FullScreenMode] Запись экрана завершена")
