"""Захват конкретного окна на Windows через Win32 API (ctypes).

Используется вместо X11-реализации program_screen.py, которая на Windows
не работает. Интерфейс класса WindowsProgramScreenMode повторяет
ProgramScreenMode: __init__(root, on_start), stop_recording(),
list_windows_with_pid(), get_window_info(), capture_window_area().

Захват окна выполняется через PrintWindow (с флагом PW_RENDERFULLCONTENT),
который корректно снимает содержимое окна, включая композитированные и
аппаратно-ускоренные части (браузеры, DX-приложения).
"""

import ctypes
import ctypes.wintypes as wt
import os
import threading
import time
import tkinter as tk
from tkinter import messagebox

import cv2
import numpy as np
from PIL import Image, ImageTk

from video_writer_utils import create_video_writer


# Win32-константы
PW_RENDERFULLCONTENT = 0x00000002
DWM_CLOAKED = 14
GWL_EXSTYLE = -20
WS_EX_TOOLWINDOW = 0x00000080
WS_EX_APPWINDOW = 0x00040000


def _user32():
    """Возвращает модуль user32 из Win32 API."""
    return ctypes.windll.user32


def _gdi32():
    """Возвращает модуль gdi32 из Win32 API."""
    return ctypes.windll.gdi32


def _dwmapi():
    """Возвращает модуль dwmapi из Win32 API."""
    return ctypes.windll.dwmapi


class _RECT(ctypes.Structure):
    """Прямоугольник в экранных координатах (левый/верхний/правый/нижний)."""

    _fields_ = [("left", wt.LONG), ("top", wt.LONG), ("right", wt.LONG), ("bottom", wt.LONG)]


def _is_visible_style(hwnd) -> bool:
    """Окна с WS_EX_TOOLWINDOW обычно не должны попадать в список (скрытые панели)."""
    u = _user32()
    try:
        ex = u.GetWindowLongW(hwnd, GWL_EXSTYLE)
    except Exception:
        ex = 0
    if ex & WS_EX_TOOLWINDOW and not ex & WS_EX_APPWINDOW:
        return False
    return True


def _is_cloaked(hwnd) -> bool:
    """Проверка, что окно не DWM-скрыто (cloaked)."""
    try:
        val = wt.DWORD(0)
        _dwmapi().DwmGetWindowAttribute(hwnd, DWM_CLOAKED, ctypes.byref(val), ctypes.sizeof(val))
        return bool(val.value)
    except Exception:
        return False


def _get_window_title(hwnd) -> str:
    """Возвращает заголовок окна по его HWND."""
    u = _user32()
    n = u.GetWindowTextLengthW(hwnd)
    buf = ctypes.create_unicode_buffer(n + 1)
    u.GetWindowTextW(hwnd, buf, n + 1)
    return buf.value


def _get_window_pid(hwnd) -> int:
    """Возвращает PID процесса, которому принадлежит окно."""
    u = _user32()
    pid = wt.DWORD(0)
    u.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value


def _list_top_windows() -> list[tuple[int, str, int]]:
    """Вернуть [(hwnd, title, pid)] видимых окон верхнего уровня."""
    u = _user32()
    result: list[tuple[int, str, int]] = []

    EnumWindowsProc = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)

    @EnumWindowsProc
    def _cb(hwnd, lparam):
        """Колбэк EnumWindows: отбирает видимые окна с заголовком и PID в список."""
        if not u.IsWindowVisible(hwnd):
            return True
        if _is_cloaked(hwnd):
            return True
        if not _is_visible_style(hwnd):
            return True
        title = _get_window_title(hwnd)
        if not title:
            return True
        pid = _get_window_pid(hwnd)
        result.append((int(hwnd), title, pid))
        return True

    u.EnumWindows(_cb, 0)
    return result


class WindowsProgramScreenMode:
    """Режим записи выбранного окна на Windows."""

    def __init__(self, root, on_start_callback, fps=20):
        """Инициализирует режим и сразу запускает показ окна выбора окна для записи."""
        self.thread = None
        self.selected_window_info = None
        self.is_recording = False
        self.fps = fps
        self.root = root
        self.on_start_callback = on_start_callback
        self.icons = []
        self._crop = None
        self.start_recording()

    # ── Список окон ──────────────────────────────────────────────────────────
    def list_windows_with_pid(self):
        """Возвращает список (HWND, заголовок, PID) видимых окон верхнего уровня."""
        return _list_top_windows()

    def get_window_info(self, hwnd):
        """Вернуть геометрию окна в экранных координатах."""
        u = _user32()
        try:
            rect = _RECT()
            u.GetWindowRect(hwnd, ctypes.byref(rect))
            w = rect.right - rect.left
            h = rect.bottom - rect.top
            x = rect.left
            y = rect.top
            info = {"x": x, "y": y, "width": w, "height": h, "window_id": hwnd}
            return info
        except Exception as e:
            print(f"Ошибка получения данных окна: {e}")
            return None

    def capture_window_area(self, window_info):
        """Захват содержимого окна через PrintWindow → numpy BGRA."""
        hwnd = window_info["window_id"]
        w = int(window_info["width"])
        h = int(window_info["height"])
        if w < 2 or h < 2:
            return None
        try:
            u = _user32()
            g = _gdi32()
            hwnddc = u.GetWindowDC(hwnd)
            memdc = g.CreateCompatibleDC(hwnddc)

            class _BITMAPINFOHEADER(ctypes.Structure):
                _fields_ = [
                    ("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
                    ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
                    ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG), ("biYPelsPerMeter", wt.LONG),
                    ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD),
                ]

            class _BITMAPINFO(ctypes.Structure):
                _fields_ = [("bmiHeader", _BITMAPINFOHEADER), ("bmiColors", wt.DWORD * 3)]

            hbmp = g.CreateCompatibleBitmap(hwnddc, w, h)
            g.SelectObject(memdc, hbmp)

            # PrintWindow с PW_RENDERFULLCONTENT снимает реальное содержимое
            ok = u.PrintWindow(hwnd, memdc, PW_RENDERFULLCONTENT)

            # Считываем пиксели
            bmi = _BITMAPINFO()
            bmi.bmiHeader.biSize = ctypes.sizeof(_BITMAPINFOHEADER)
            bmi.bmiHeader.biWidth = w
            bmi.bmiHeader.biHeight = -h  # top-down
            bmi.bmiHeader.biPlanes = 1
            bmi.bmiHeader.biBitCount = 32
            bmi.bmiHeader.biCompression = 0  # BI_RGB

            buf = ctypes.create_string_buffer(w * h * 4)
            got = g.GetDIBits(memdc, hbmp, 0, h, buf, ctypes.byref(bmi), 0)

            image = np.frombuffer(buf.raw, dtype=np.uint8).reshape((h, w, 4)) if got else None

            g.DeleteObject(hbmp)
            g.DeleteDC(memdc)
            u.ReleaseDC(hwnd, hwnddc)

            if image is None:
                print(f"PrintWindow вернул пустой захват (ok={ok})")
                return None
            return image
        except Exception as e:
            print(f"Ошибка при захвате окна: {e}")
            return None

    # ── UI выбора окна ───────────────────────────────────────────────────────
    def start_recording(self):
        """Показывает окно со списком доступных окон; по выбору окна запускает запись."""
        windows = self.list_windows_with_pid()
        if not windows:
            messagebox.showinfo("Нет окон", "Нет доступных окон для захвата.")
            self._end_select(recording_started=False)
            return

        sel_root = tk.Toplevel(self.root)
        sel_root.title("Выберите окно для записи")
        sel_root.transient(self.root)
        sel_root.lift()
        canvas = tk.Canvas(sel_root, bg="#282828")
        scrollbar = tk.Scrollbar(sel_root, orient="vertical", command=canvas.yview)
        scroll_frame = tk.Frame(canvas, bg="#282828")
        scroll_frame.bind(
            "<Configure>",
            lambda _e: canvas.configure(scrollregion=canvas.bbox("all")),
        )
        canvas.create_window((0, 0), window=scroll_frame, anchor="nw")
        canvas.configure(yscrollcommand=scrollbar.set)
        canvas.pack(side="left", fill="both", expand=True)
        scrollbar.pack(side="right", fill="y")

        def _on_mousewheel(event, canvas=canvas):
            """Прокручивает список окон колёсиком мыши."""
            try:
                if event.delta:
                    canvas.yview_scroll(-1 * int(event.delta / 120), "units")
                elif event.num == 4:
                    canvas.yview_scroll(-1, "units")
                elif event.num == 5:
                    canvas.yview_scroll(1, "units")
                return "break"
            except Exception:
                return None

        sel_root.bind("<MouseWheel>", _on_mousewheel)
        sel_root.bind("<Button-4>", _on_mousewheel)
        sel_root.bind("<Button-5>", _on_mousewheel)

        def select_window(hwnd):
            """Обрабатывает выбор окна: получает геометрию, закрывает окно и запускает запись."""
            win_info = self.get_window_info(hwnd)
            try:
                sel_root.grab_release()
            except Exception:
                pass
            try:
                sel_root.unbind("<MouseWheel>")
                sel_root.unbind("<Button-4>")
                sel_root.unbind("<Button-5>")
            except Exception:
                pass
            sel_root.destroy()
            if not win_info:
                messagebox.showerror("Ошибка", "Не удалось получить параметры окна.")
                self._end_select(recording_started=False)
                return
            self.selected_window_info = win_info
            self.is_recording = True
            self._end_select(recording_started=True)
            self._run_record_thread(win_info)

        for hwnd, title, pid in windows:
            img = Image.new("RGBA", (36, 36), (80, 80, 80, 255))
            tkimg = ImageTk.PhotoImage(img)
            self.icons.append(tkimg)
            btn = tk.Button(
                scroll_frame,
                image=tkimg,
                text=title if len(title) < 60 else title[:57] + "...",
                compound="left",
                anchor="w",
                font=("Arial", 11),
                bg="#353535", fg="#cccaca",
                padx=10,
                command=lambda wid=hwnd: select_window(wid),
            )
            btn.pack(fill="x", pady=4, padx=8)

        sel_root.update_idletasks()
        frame_width = scroll_frame.winfo_reqwidth()
        frame_height = scroll_frame.winfo_reqheight()
        win_w = min(max(frame_width + scrollbar.winfo_width(), 300), 400)
        win_h = min(max(frame_height, 200), 500)
        self.root.update_idletasks()
        root_x = self.root.winfo_rootx()
        root_y = self.root.winfo_rooty()
        root_w = self.root.winfo_width()
        root_h = self.root.winfo_height()
        pos_x = root_x + max(0, (root_w - win_w) // 2)
        pos_y = root_y + max(0, (root_h - win_h) // 2)
        sel_root.geometry(f"{win_w}x{win_h}+{pos_x}+{pos_y}")

        try:
            sel_root.wait_visibility()
            sel_root.grab_set()
        except Exception:
            pass

        def on_close():
            try:
                sel_root.grab_release()
            except Exception:
                pass
            sel_root.unbind_all("<MouseWheel>")
            sel_root.unbind_all("<Button-4>")
            sel_root.unbind_all("<Button-5>")
            sel_root.destroy()
            self._end_select(recording_started=False)

        sel_root.protocol("WM_DELETE_WINDOW", on_close)

    # ── Поток записи ─────────────────────────────────────────────────────────
    def _run_record_thread(self, window_info):
        """Запускает фоновый поток записи выбранного окна в видеофайл."""
        filename = "output.mkv"
        raw_w = int(window_info["width"])
        raw_h = int(window_info["height"])
        if raw_w < 2 or raw_h < 2:
            print("[WindowsProgramScreenMode] Ошибка: размер окна слишком мал")
            self.is_recording = False
            return

        def record():
            """Тело потока записи: снимает окно с заданным FPS и пишет кадры до остановки."""
            print(f"Запись окна {hex(window_info['window_id'])} в файл {filename}")
            try:
                out, (target_w, target_h), codec = create_video_writer(filename, self.fps, (raw_w, raw_h))
                print(f"[WindowsProgramScreenMode] Кодек {codec} для {target_w}x{target_h}")
            except Exception as exc:
                print(f"[WindowsProgramScreenMode] Ошибка инициализации VideoWriter: {exc}")
                self.is_recording = False
                return
            frame_interval = 1.0 / max(self.fps, 1)
            next_frame_time = time.perf_counter()
            while self.is_recording:
                now = time.perf_counter()
                if now < next_frame_time:
                    time.sleep(next_frame_time - now)

                frame = self.capture_window_area(window_info)
                if frame is not None:
                    fh, fw = frame.shape[:2]
                    if fw != target_w or fh != target_h:
                        frame = cv2.resize(frame, (target_w, target_h), interpolation=cv2.INTER_AREA)
                    bgr = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)
                    out.write(bgr)
                next_frame_time += frame_interval
                if time.perf_counter() - next_frame_time > frame_interval:
                    next_frame_time = time.perf_counter()
            out.release()
            print("[WindowsProgramScreenMode] Запись окна завершена")

        self.thread = threading.Thread(target=record, daemon=True)
        self.thread.start()

    def _end_select(self, recording_started: bool):
        """Завершает выбор окна и вызывает колбэк о том, стартовала ли запись."""
        if self.on_start_callback:
            self.on_start_callback(recording_started)

    def stop_recording(self):
        """Останавливает запись окна, дождавшись завершения фонового потока."""
        self.is_recording = False
        if self.thread and self.thread.is_alive():
            self.thread.join(timeout=2.0)
        print("Остановка записи окна.")
