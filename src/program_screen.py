import os
import subprocess
import threading
import time
import tkinter as tk
from tkinter import messagebox

import cv2
import numpy as np
from PIL import Image, ImageTk
from Xlib import X, Xatom, display, error

from video_writer_utils import create_video_writer


def get_icon_for_pid(pid):
    """Находит путь к значку программы по её PID (через /proc и стандартные каталоги иконок)."""
    exe_path = None
    icon_path = None
    try:
        exe_path = os.readlink(f"/proc/{pid}/exe")
    except Exception:
        pass
    if exe_path:
        app_name = os.path.basename(exe_path)
        icon_name = app_name.split()[0]
        for icon_dir in [
            "/usr/share/pixmaps",
            "/usr/share/icons/hicolor/48x48/apps",
            "/usr/share/icons/hicolor/64x64/apps",
            "/usr/share/icons/hicolor/128x128/apps",
            "/usr/share/icons/Adwaita/48x48/apps",
            "/usr/share/icons/Adwaita/64x64/apps",
            "/usr/share/icons/Adwaita/128x128/apps",
            "/usr/share/icons/gnome/48x48/apps",
            "/usr/share/icons/gnome/64x64/apps",
        ]:
            for ext in ["png", "svg", "xpm"]:
                test_path = os.path.join(icon_dir, f"{icon_name}.{ext}")
                if os.path.exists(test_path):
                    icon_path = test_path
                    break
            if icon_path:
                break
    return icon_path

def get_icon_from_window(win_id):
    """Достаёт иконку окна из атрибута _NET_WM_ICON (самую крупную) как PIL-изображение."""
    try:
        d = display.Display()
        window = d.create_resource_object('window', win_id)
        atom = d.intern_atom('_NET_WM_ICON')
        prop = window.get_full_property(atom, X.AnyPropertyType)
        if prop:
            data = prop.value
            icons = []
            idx = 0
            while idx < len(data) - 1:
                width = data[idx]
                height = data[idx+1]
                size = width * height
                if idx + 2 + size > len(data):
                    break
                arr = np.array(data[idx+2: idx+2+size], dtype=np.uint32).reshape((height, width))
                icons.append(arr)
                idx += 2 + size
            if icons:
                arr = max(icons, key=lambda i: i.shape[0]*i.shape[1])
                img = Image.fromarray(np.uint8(np.stack([(arr >> 16) & 255, (arr >> 8) & 255, arr & 255, (arr >> 24) & 255], axis=-1)))
                return img
    except Exception as e:
        print(f"Ошибка получения иконки из окна: {e}")
        return None

class ProgramScreenMode:
    """Режим записи выбранного окна на X11: список окон, захват и запись."""

    def __init__(self, root, on_start_callback, fps=20):
        """Инициализирует режим и сразу запускает показ окна выбора окна для записи."""
        self.thread = None
        self.selected_window_info = None
        self.is_recording = False
        self.fps = fps
        self.root = root
        self.on_start_callback = on_start_callback
        self.icons = []
        self.start_recording()

    def list_windows_with_pid(self):
        """Возвращает список (id, заголовок, PID) окон: через X11, а при неудаче — wmctrl."""
        windows = self._list_windows_via_x11()
        if windows:
            return windows
        return self._list_windows_via_wmctrl()

    def _list_windows_via_x11(self) -> list[tuple[int, str, int]]:
        """Собирает видимые окна с PID и заголовком напрямую через Xlib."""
        try:
            d = display.Display()
        except Exception as exc:
            print(f"[ProgramScreenMode] Не удалось подключиться к X11: {exc}")
            return []

        windows: list[tuple[int, str, int]] = []
        try:
            root = d.screen().root
            window_ids: list[int] = []
            for atom_name in ("_NET_CLIENT_LIST_STACKING", "_NET_CLIENT_LIST"):
                try:
                    atom = d.intern_atom(atom_name, True)
                except Exception:
                    atom = None
                if not atom:
                    continue
                prop = root.get_full_property(atom, X.AnyPropertyType)
                if prop and getattr(prop, "value", None):
                    window_ids = [int(win_id) for win_id in prop.value]
                    if window_ids:
                        break

            if not window_ids:
                try:
                    window_ids = [child.id for child in root.query_tree().children]
                except Exception as exc:
                    print(f"[ProgramScreenMode] Не удалось получить дочерние окна: {exc}")
                    window_ids = []

            try:
                pid_atom = d.intern_atom("_NET_WM_PID", True)
            except Exception:
                pid_atom = None

            name_atoms: list[int] = []
            try:
                name_atoms.append(d.intern_atom("_NET_WM_NAME", True))
            except Exception:
                name_atoms.append(0)
            name_atoms.append(Xatom.WM_NAME)

            for win_id in window_ids:
                try:
                    window = d.create_resource_object('window', win_id)
                    attrs = window.get_attributes()
                    if attrs.map_state != X.IsViewable:
                        continue

                    pid = None
                    if pid_atom:
                        pid_prop = window.get_full_property(pid_atom, X.AnyPropertyType)
                        if pid_prop and getattr(pid_prop, "value", None):
                            try:
                                pid = int(pid_prop.value[0])
                            except Exception:
                                pid = None

                    if not pid or pid <= 0:
                        continue

                    title = self._extract_window_title(window, name_atoms)
                    if not title:
                        continue

                    windows.append((win_id, title, pid))
                except error.XError:
                    continue
                except Exception as exc:
                    print(f"[ProgramScreenMode] Ошибка обработки окна {hex(win_id)}: {exc}")

            return windows
        finally:
            try:
                d.close()
            except Exception:
                pass

    def _extract_window_title(self, window, name_atoms: list[int]) -> str:
        """Извлекает заголовок окна, перебирая переданные атрибуты имени (до первого заполненного)."""
        title = ""
        for atom in name_atoms:
            if not atom:
                continue
            try:
                prop = window.get_full_property(atom, X.AnyPropertyType)
            except error.XError:
                continue
            if not prop or not getattr(prop, "value", None):
                continue
            value = prop.value
            if isinstance(value, bytes):
                title = value.decode('utf-8', 'ignore').strip()
            elif isinstance(value, str):
                title = value.strip()
            else:
                try:
                    title = bytes(value).decode('utf-8', 'ignore').strip()
                except Exception:
                    title = ""
            if title:
                break

        if not title:
            try:
                raw_title = window.get_wm_name()
                if isinstance(raw_title, bytes):
                    title = raw_title.decode('utf-8', 'ignore').strip()
                elif isinstance(raw_title, str):
                    title = raw_title.strip()
            except Exception:
                title = ""

        return title

    def _list_windows_via_wmctrl(self) -> list[tuple[int, str, int]]:
        """Возвращает список (id, заголовок, PID) окон через утилиту wmctrl."""
        try:
            output = subprocess.check_output(['wmctrl', '-lp'], stderr=subprocess.DEVNULL)
        except FileNotFoundError:
            print("[ProgramScreenMode] Утилита wmctrl не найдена")
            return []
        except subprocess.CalledProcessError as exc:
            print(f"[ProgramScreenMode] Ошибка запуска wmctrl: {exc}")
            return []
        except Exception as exc:
            print(f"[ProgramScreenMode] Не удалось получить список окон через wmctrl: {exc}")
            return []

        windows: list[tuple[int, str, int]] = []
        for raw_line in output.decode('utf-8', 'ignore').splitlines():
            parts = raw_line.split(None, 4)
            if len(parts) != 5:
                continue
            try:
                win_id = int(parts[0], 16)
                pid = int(parts[2])
            except Exception:
                continue
            title = parts[4].strip()
            if title and pid > 0:
                windows.append((win_id, title, pid))
        return windows

    def get_window_info(self, win_id):
        """Возвращает геометрию окна в экранных координатах (с учётом вложенных родителей)."""
        try:
            d = display.Display()
            window = d.create_resource_object('window', win_id)
            attrs = window.get_geometry()
            x = attrs.x
            y = attrs.y
            w = attrs.width
            h = attrs.height
            try:
                tree = window.query_tree()
                parent = tree.parent
                tx, ty = x, y
                while parent:
                    parent_attrs = parent.get_geometry()
                    tx += parent_attrs.x
                    ty += parent_attrs.y
                    if parent.id == d.screen().root.id:
                        break
                    parent = parent.query_tree().parent
                x, y = tx, ty
            except Exception:
                pass
            info = {'x': x, 'y': y, 'width': w, 'height': h, 'window_id': win_id}
            print(f"window_info: {info}")
            return info
        except Exception as e:
            print(f"Ошибка получения данных окна: {e}")
            return None

    def capture_window_area(self, window_info):
        """Снимает содержимое окна через XGetImage и возвращает numpy-массив BGRA."""
        try:
            d = display.Display()
            window = d.create_resource_object('window', window_info['window_id'])
            geometry = window.get_geometry()
            w = geometry.width
            h = geometry.height
            raw_image = window.get_image(0, 0, w, h, X.ZPixmap, 0xffffffff)
            image = np.frombuffer(raw_image.data, dtype=np.uint8)
            image = image.reshape((h, w, 4))
            return image
        except Exception as e:
            print(f"Ошибка при захвате окна: {e}")
            return None

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
            lambda _e: canvas.configure(
                scrollregion=canvas.bbox("all")
            )
        )
        canvas.create_window((0, 0), window=scroll_frame, anchor="nw")
        canvas.configure(yscrollcommand=scrollbar.set)
        canvas.pack(side="left", fill="both", expand=True)
        scrollbar.pack(side="right", fill="y")

        def _on_mousewheel(event, canvas=canvas):
            """Прокручивает список окон колёсиком мыши."""
            # Обработка прокрутки мыши
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

        # Обработка прокрутки колеса мыши - bind к главному окну
        sel_root.bind("<MouseWheel>", _on_mousewheel)
        sel_root.bind("<Button-4>", _on_mousewheel)
        sel_root.bind("<Button-5>", _on_mousewheel)

        def select_window(winid):
            """Обрабатывает выбор окна: получает геометрию, закрывает окно и запускает запись."""
            win_info = self.get_window_info(winid)
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

        for winid, title, pid in windows:
            img = get_icon_from_window(winid)
            if img:
                img = img.resize((36, 36), Image.Resampling.LANCZOS)
                tkimg = ImageTk.PhotoImage(img)
            else:
                icon_path = get_icon_for_pid(pid)
                if icon_path and os.path.exists(icon_path):
                    try:
                        img = Image.open(icon_path).resize((36, 36), Image.Resampling.LANCZOS)
                        tkimg = ImageTk.PhotoImage(img)
                    except Exception:
                        img = Image.new("RGBA", (36, 36), (80, 80, 80, 255))
                        tkimg = ImageTk.PhotoImage(img)
                else:
                    img = Image.new("RGBA", (36, 36), (80, 80, 80, 255))
                    tkimg = ImageTk.PhotoImage(img)
            self.icons.append(tkimg)
            btn = tk.Button(
                scroll_frame,
                image=tkimg,
                text=title if len(title)<60 else title[:57]+'...',
                compound="left",
                anchor="w",
                font=("Arial", 11),
                bg="#353535", fg="#cccaca",
                padx=10,
                command=lambda wid=winid: select_window(wid)
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

    def _run_record_thread(self, window_info):
        """Запускает фоновый поток записи выбранного окна в видеофайл."""
        filename = "output.mkv"
        raw_w = int(window_info['width'])
        raw_h = int(window_info['height'])
        if raw_w < 2 or raw_h < 2:
            print("[ProgramScreenMode] Ошибка: размер окна слишком мал для записи")
            self.is_recording = False
            return

        def record():
            """Тело потока записи: снимает окно с заданным FPS и пишет кадры до остановки."""
            print(f"Запись области окна {hex(window_info['window_id'])} начата в файл {filename}")
            try:
                out, (target_w, target_h), codec = create_video_writer(filename, self.fps, (raw_w, raw_h))
                print(f"[ProgramScreenMode] Используется кодек {codec} для размеров {target_w}x{target_h}")
            except Exception as exc:
                print(f"[ProgramScreenMode] Ошибка инициализации VideoWriter: {exc}")
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
                    rgb_frame = cv2.cvtColor(frame, cv2.COLOR_BGRA2BGR)
                    out.write(rgb_frame)

                next_frame_time += frame_interval
                if time.perf_counter() - next_frame_time > frame_interval:
                    next_frame_time = time.perf_counter()
            out.release()
            print("[ProgramScreenMode] Запись окна завершена")

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
