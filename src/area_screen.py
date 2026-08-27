import asyncio
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import tkinter as tk
from io import BytesIO
from urllib.parse import unquote, urlparse

import cv2
import numpy as np
from PIL import Image, ImageGrab, ImageTk

from video_writer_utils import create_video_writer


def _is_wayland() -> bool:
    if os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland":
        return True
    return bool(os.environ.get("WAYLAND_DISPLAY"))


def _portal_screenshot_bytes(timeout: float = 180.0, parent_window: str = "") -> bytes | None:
    """Скриншот всего экрана через xdg-desktop-portal Screenshot (dbus-next).

    Работает в frozen AppImage без PyGObject. На GNOME портал показывает
    диалог разрешения — ожидаем ответ пользователя до timeout секунд.
    parent_window — хэндл окна-родителя ("x11:0x…" либо ""), к которому
    GNOME привязывает диалог доступа; без него диалог может быть отменён.
    Возвращает PNG-байты или None.
    """
    try:
        return asyncio.run(_portal_screenshot_with_retry_async(timeout, parent_window))
    except Exception:
        return None


async def _clear_screenshot_denial() -> None:
    """Удалить сохранённый запрет на скриншоты из PermissionStore.

    При отмене диалога GNOME может записать «no» для приложения (или для
    пустого id — глобально), тогда все последующие запросы отклоняются
    мгновенно без диалога. Очищаем и пустой id, и собственный app-id.
    """
    from dbus_next.aio import MessageBus
    from dbus_next.message import Message

    ids = ["", "io.github.freekazoid.recordingscreen"]
    try:
        from portal_identity import APP_ID

        ids.append(APP_ID)
    except Exception:
        pass
    ids = list(dict.fromkeys(ids))

    bus = await MessageBus().connect()
    try:
        for app_id in ids:
            try:
                await bus.call(
                    Message(
                        destination="org.freedesktop.impl.portal.PermissionStore",
                        path="/org/freedesktop/impl/portal/PermissionStore",
                        interface="org.freedesktop.impl.portal.PermissionStore",
                        member="DeletePermission",
                        signature="sss",
                        body=["screenshot", app_id, "screenshot"],
                    )
                )
            except Exception:
                pass
        _dbg(f"портал: очистили запреты для id={ids}")
    finally:
        try:
            bus.disconnect()
        except Exception:
            pass


#: Ответ считается «мгновенным автоматическим отказом» быстрее этого порога.
_INSTANT_DENY_SEC = 2.0


async def _portal_screenshot_attempt(
    timeout: float,
    parent_window: str = "",
) -> tuple[bytes | None, int | None]:
    """Одна попытка портал-скриншота: (PNG-байты, код ответа портала)."""
    from dbus_next.aio import MessageBus
    from dbus_next.message import Message, MessageType
    from dbus_next import Variant

    bus = await MessageBus().connect()
    code: int | None = 1
    try:
        token = f"srec_shot_{os.getpid()}_{time.monotonic_ns() % 100000}"
        sender = (bus.unique_name or "").lstrip(":").replace(".", "_")
        req_path = f"/org/freedesktop/portal/desktop/request/{sender}/{token}"
        loop = asyncio.get_running_loop()
        wait_response: asyncio.Future = loop.create_future()

        def _on_message(msg):
            if (
                msg.path == req_path
                and msg.member == "Response"
                and not wait_response.done()
            ):
                wait_response.set_result(msg.body)

        bus.add_message_handler(_on_message)
        reply = await asyncio.wait_for(
            bus.call(
                Message(
                    destination="org.freedesktop.portal.Desktop",
                    interface="org.freedesktop.portal.Screenshot",
                    path="/org/freedesktop/portal/desktop",
                    member="Screenshot",
                    signature="sa{sv}",
                    body=[
                        parent_window,
                        {
                            "handle_token": Variant("s", token),
                            "interactive": Variant("b", False),
                        },
                    ],
                )
            ),
            timeout=timeout,
        )
        if reply.message_type != MessageType.METHOD_RETURN:
            code = 1
            return None, code
        code_raw, results = await asyncio.wait_for(wait_response, timeout=timeout)
        try:
            code = int(code_raw)
        except Exception:
            code = 1
        if code != 0:
            return None, code
        uri_variant = (results or {}).get("uri")
        uri = getattr(uri_variant, "value", uri_variant)
        if not uri or not str(uri).startswith("file://"):
            return None, code
        # URI может быть URL-кодирован (например /%D0%98%D0%B7%D0%BE... для «Изображения»)
        png_path = unquote(urlparse(str(uri)).path)
        _dbg(f"портал: скриншот получен, uri={str(uri)}, файл={png_path}")

        def _read_and_cleanup() -> bytes:
            with open(png_path, "rb") as f:
                data = f.read()
            try:
                os.unlink(png_path)
            except OSError:
                pass
            return data

        data = await loop.run_in_executor(None, _read_and_cleanup)
        return data, code
    finally:
        try:
            bus.disconnect()
        except Exception:
            pass


def _dbg(message: str) -> None:
    """Диагностика выбора области: пишем причины неудач в logs/area_select.log."""
    try:
        from logging_utils import write_error_report

        write_error_report("area_select", message)
    except Exception:
        pass


async def _portal_screenshot_with_retry_async(
    timeout: float, parent_window: str = ""
) -> bytes | None:
    """Попытка скриншота; при мгновенном автоотказе чистим запрет и повторяем."""
    started = time.monotonic()
    try:
        data, code = await _portal_screenshot_attempt(timeout, parent_window)
    except Exception as exc:
        _dbg(f"портал: попытка 1 упала: {type(exc).__name__}: {exc}")
        raise
    elapsed = time.monotonic() - started
    if data is None:
        _dbg(
            f"портал: попытка 1 без результата за {elapsed:.2f}с "
            f"(code={code}, wayland={_is_wayland()}, parent={parent_window!r})"
        )
    if data is None and code is not None and code != 0 and elapsed < _INSTANT_DENY_SEC:
        # Мгновенный отказ: обычно нет сохранённого разрешения либо запрос
        # ушёл без фокуса. Чистим сохранённый запрет и повторяем; повторный
        # запрос из окна выбора (в фокусе) покажет диалог GNOME.
        _dbg("портал: мгновенный отказ — чищу PermissionStore и повторяю")
        await _clear_screenshot_denial()
        try:
            data, code = await _portal_screenshot_attempt(timeout, parent_window)
        except Exception as exc:
            _dbg(f"портал: попытка 2 упала: {type(exc).__name__}: {exc}")
            return None
        if data is None:
            _dbg(f"портал: попытка 2 без результата (code={code})")
    return data


def _grim_screenshot_bytes() -> bytes | None:
    """Скриншот через grim (стандартный инструмент wlroots-композиторов)."""
    if shutil.which("grim") is None:
        return None
    try:
        with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as tmp:
            tmp_path = tmp.name
        proc = subprocess.run(
            ["grim", "-t", "png", tmp_path],
            capture_output=True,
            timeout=15,
            check=False,
        )
        if proc.returncode != 0:
            return None
        with open(tmp_path, "rb") as f:
            return f.read()
    except Exception:
        return None
    finally:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass


#: Отменил ли пользователь диалог доступа к экрану (при этом окно не открываем).
_last_grab_cancelled = False


def _grab_background_image() -> Image.Image | None:
    """Кадр экрана для подложки выбора области.

    На Wayland используется долгоживущая ScreenCast-сессия (диалог доступа
    показывается только один раз за процесс; далее кадры снимаются без
    запросов — свежие при каждом выборе). На X11 — обычный ImageGrab.
    При отмене пользователем диалога выставляет флаг «отменено».
    """
    global _last_grab_cancelled
    _last_grab_cancelled = False

    frozen = getattr(sys, "frozen", False)
    _dbg(f"старт: wayland={_is_wayland()}, frozen={frozen}")

    if _is_wayland():
        try:
            from screencast_frame import grab_screencast_frame

            fres = grab_screencast_frame()
        except Exception as exc:
            fres = None
            _dbg(f"screencast_frame: исключение {type(exc).__name__}: {exc}")
        if fres is not None and not fres.ok:
            if fres.cancelled:
                _last_grab_cancelled = True
                _dbg("подложка: доступ к экрану отменён пользователем")
                return None
            _dbg(f"screencast_frame: ошибка: {fres.error}")
        if fres is not None and fres.data:
            _dbg("подложка: кадр получен через ScreenCast-сессию")
            return _decode_png(fres.data, "screencast_frame")
        # Попытка через портал скриншотов (для систем, где он доступен).
        try:
            data = _portal_screenshot_bytes()
        except Exception as exc:
            data = None
            _dbg(f"портал: исключение {type(exc).__name__}: {exc}")
        if data:
            return _decode_png(data, "портал")
        try:
            data = _grim_screenshot_bytes()
        except Exception as exc:
            data = None
            _dbg(f"grim: исключение {type(exc).__name__}: {exc}")
        if data:
            return _decode_png(data, "grim")

    try:
        result = ImageGrab.grab()
        if result is not None:
            _dbg("ImageGrab: использован (не Wayland или порталы недоступны)")
        return result
    except Exception as exc:
        _dbg(f"ImageGrab: исключение {type(exc).__name__}: {exc}")
        return None


def _decode_png(data: bytes, source: str) -> Image.Image | None:
    try:
        img = Image.open(BytesIO(data))
        img.load()
        return img.convert("RGB")
    except Exception as exc:
        _dbg(f"{source}: PNG не декодировался: {type(exc).__name__}: {exc}")
        return None


def select_wayland_area() -> tuple[int, int, int, int] | None:
    if os.environ.get("XDG_SESSION_TYPE", "").lower() != "wayland" and not os.environ.get("WAYLAND_DISPLAY"):
        return None
    if shutil.which("slurp") is None:
        return None
    try:
        proc = subprocess.run(
            ["slurp", "-f", "%x,%y,%w,%h"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        if proc.returncode != 0:
            return None
        raw = (proc.stdout or "").strip()
        parts = raw.split(",")
        if len(parts) != 4:
            return None
        x, y, w, h = [int(part) for part in parts]
        if w <= 5 or h <= 5:
            return None
        return (x, y, x + w, y + h)
    except Exception:
        return None


def select_screen_area(master=None) -> tuple[int, int, int, int] | None:
    # Порядок: сначала скриншот из программы (главное окно ещё в фокусе —
    # портал показывает диалог), а после получения кадра — окно выбора со
    # сделанным скриншотом. Если пользователь отменил доступ к экрану —
    # окно выбора НЕ открываем.
    pil_img = _grab_background_image()
    if pil_img is None and _last_grab_cancelled:
        _dbg("подложка: пользователь отменил доступ — выбор области не открываем")
        return None
    if pil_img is not None:
        try:
            pil_img.size
        except Exception:
            pil_img = None

    root = tk.Toplevel(master)
    sw0 = max(int(root.winfo_screenwidth()), 1)
    sh0 = max(int(root.winfo_screenheight()), 1)
    root.geometry(f"{sw0}x{sh0}+0+0")
    root.attributes("-fullscreen", True)
    root.attributes("-topmost", True)
    root.config(bg="black", highlightthickness=0, borderwidth=0)
    root.bind("<Escape>", lambda _: root.destroy())
    root.protocol("WM_DELETE_WINDOW", root.destroy)
    root.update_idletasks()
    root.wait_visibility()
    root.grab_set()
    root.focus_force()

    canvas = tk.Canvas(
        root,
        width=sw0,
        height=sh0,
        cursor="cross",
        bg="black",
        highlightthickness=0,
        borderwidth=0,
    )
    canvas.pack(fill=tk.BOTH, expand=True)
    canvas.bind("<Escape>", lambda _: root.destroy())

    canvas.create_text(
        16,
        12,
        anchor="nw",
        text="Выделите область записи мышью · Esc — отмена",
        fill="#ffffff",
        font=("Arial", 12),
    )

    def _draw_background(img: Image.Image | None) -> bool:
        if img is None:
            return False
        try:
            bg_img = img.convert("RGB")
            sw = max(int(root.winfo_screenwidth()), 1)
            sh = max(int(root.winfo_screenheight()), 1)
            if bg_img.size != (sw, sh):
                _dbg(f"подложка: масштабирую {bg_img.size} -> {(sw, sh)}")
                bg_img = bg_img.resize((sw, sh), Image.BILINEAR)
            tk_img = ImageTk.PhotoImage(bg_img)
            img_id = canvas.create_image(0, 0, anchor="nw", image=tk_img)
            canvas.tag_lower(img_id)
            root._bg_img_ref = tk_img  # type: ignore[attr-defined]
            import numpy as _np

            _dbg(
                f"подложка: отрисована {bg_img.size}, "
                f"средняя яркость {float(_np.asarray(bg_img.convert('L')).mean()):.0f}/255"
            )
            return True
        except Exception as exc:
            _dbg(f"подложка: не удалось отрисовать скриншот: {type(exc).__name__}: {exc}")
            return False

    if pil_img is None:
        # Кадр не получен до открытия окна — ещё одна попытка после
        # отрисовки окна (сессия могла ещё не успеть открыться).
        _dbg("подложка: повторная попытка кадра из окна выбора")
        root.update_idletasks()
        retry_img = _grab_background_image()
        if _last_grab_cancelled:
            _dbg("подложка: доступ отменён при повторной попытке — окно закрываем")
            root.destroy()
            return None
        pil_img = retry_img
    if pil_img is None:
        _dbg("подложка: кадр экрана получить не удалось — будет чёрный фон")
    else:
        _draw_background(pil_img)

    state = {
        "start_x": None,
        "start_y": None,
        "rect": None,
        "area": None,
    }

    def on_press(event):
        state["start_x"] = canvas.canvasx(event.x)
        state["start_y"] = canvas.canvasy(event.y)
        if state["rect"]:
            canvas.delete(state["rect"])
        state["rect"] = canvas.create_rectangle(
            state["start_x"],
            state["start_y"],
            state["start_x"],
            state["start_y"],
            outline="red",
            width=2,
        )

    def on_drag(event):
        if state["rect"] is not None and state["start_x"] is not None and state["start_y"] is not None:
            cur_x = canvas.canvasx(event.x)
            cur_y = canvas.canvasy(event.y)
            canvas.coords(state["rect"], state["start_x"], state["start_y"], cur_x, cur_y)

    def on_release(event):
        if state["start_x"] is None or state["start_y"] is None:
            return
        end_x = canvas.canvasx(event.x)
        end_y = canvas.canvasy(event.y)
        x0, y0 = int(min(state["start_x"], end_x)), int(min(state["start_y"], end_y))
        x1, y1 = int(max(state["start_x"], end_x)), int(max(state["start_y"], end_y))
        if x1 - x0 > 5 and y1 - y0 > 5:
            state["area"] = (x0, y0, x1, y1)
            root.destroy()

    canvas.bind("<ButtonPress-1>", on_press)
    canvas.bind("<B1-Motion>", on_drag)
    canvas.bind("<ButtonRelease-1>", on_release)
    root.wait_window()
    return state["area"]


class AreaScreenMode:
    def __init__(self, master=None):
        self.master = master
        self.selected_area: tuple[int, int, int, int] | None = select_screen_area(master)
        self.recording = False
        self._thread = None
        self._stop_event = threading.Event()
        self.video_filepath: str | None = None

    def start_recording(self):
        if self.selected_area:
            print(f"[AreaScreenMode] Начата запись области: {self.selected_area}")
            self.recording = True
            self._stop_event.clear()
            self.video_filepath = "output.mkv"
            self._thread = threading.Thread(target=self._capture_loop, daemon=True)
            self._thread.start()
        else:
            print("[AreaScreenMode] Область не выбрана, запись не начата")

    def _capture_loop(self):
        if self.selected_area is None or self.video_filepath is None:
            print("[AreaScreenMode] selected_area или video_filepath не определены!")
            return
        x0, y0, x1, y1 = self.selected_area
        width = x1 - x0
        height = y1 - y0
        fps = 12.0
        frame_interval = 1.0 / fps
        try:
            out, (target_w, target_h), codec = create_video_writer(self.video_filepath, fps, (width, height))
            print(f"[AreaScreenMode] Используется кодек {codec} для размеров {target_w}x{target_h}")
        except Exception as exc:
            print(f"[AreaScreenMode] Ошибка инициализации VideoWriter: {exc}")
            self.recording = False
            return
        next_frame_time = time.perf_counter()

        while not self._stop_event.is_set():
            now = time.perf_counter()
            if now < next_frame_time:
                time.sleep(next_frame_time - now)

            # ImageGrab is unreliable on Wayland; retry-safe capture
            try:
                img = ImageGrab.grab(bbox=self.selected_area)
                frame = cv2.cvtColor(np.array(img), cv2.COLOR_RGB2BGR)
            except Exception:
                continue
            if frame.shape[1] != target_w or frame.shape[0] != target_h:
                frame = cv2.resize(frame, (target_w, target_h), interpolation=cv2.INTER_AREA)
            # Ensure writer is valid before writing
            if out is not None and out.isOpened():
                out.write(frame)

            next_frame_time += frame_interval
            if time.perf_counter() - next_frame_time > frame_interval:
                next_frame_time = time.perf_counter()

        out.release()
        print(f"[AreaScreenMode] Видео сохранено в {self.video_filepath}")

    def stop_recording(self):
        if self.recording:
            print("[AreaScreenMode] Остановлена запись области")
            self._stop_event.set()
            if self._thread and self._thread.is_alive():
                self._thread.join(timeout=1.5)
        self.recording = False
