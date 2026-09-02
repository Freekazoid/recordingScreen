import os
import sys

import pystray
from PIL import Image
from pystray import MenuItem as Item


def create_tray_icon(root, appwin):
    """Создаёт и запускает иконку системного трея с меню управления записью."""
    os.environ["PYGOBJECT_FORCE_GTK3"] = "0"

    menu = pystray.Menu(
        Item("Показать окно", _on_show),
        Item("Скрыть окно", _on_hide),
        pystray.Menu.SEPARATOR,
        Item("Весь экран", _on_fullscreen),
        Item("Область", _on_area),
        Item("Программа", _on_program),
        Item("Стоп", _on_stop),
        pystray.Menu.SEPARATOR,
        Item("Выход", _on_quit),
    )

    try:
        if getattr(sys, "frozen", False):
            base_dir = getattr(sys, "_MEIPASS", os.path.dirname(sys.executable))
        else:
            base_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        img_path = os.path.join(base_dir, "assets", "icon.png")
        img = Image.open(img_path).resize((32, 32), Image.Resampling.LANCZOS)
    except Exception:
        img = Image.new("RGBA", (32, 32), (255, 0, 0, 255))

    icon = pystray.Icon("screenrecorder", img, "Screen Recorder", menu)
    icon.run()


_tray_root = None
_tray_app = None


def _on_show(icon, item):
    """Обработчик меню «Показать окно»: разворачивает и показывает главное окно."""
    if _tray_root and _tray_root.winfo_exists():
        _tray_root.after(0, _show_win)


def _show_win():
    """Показывает и поднимает главное окно из трея."""
    if _tray_root and _tray_root.winfo_exists():
        _tray_root.state("normal")
        _tray_root.deiconify()
        _tray_root.lift()


def _on_hide(icon, item):
    """Обработчик меню «Скрыть окно»: сворачивает главное окно в трей."""
    if _tray_root and _tray_root.winfo_exists():
        _tray_root.withdraw()


def _on_fullscreen(icon, item):
    """Обработчик меню «Запись всего экрана»."""
    if _tray_root:
        _tray_root.after(0, _tray_app.on_full_screen)


def _on_area(icon, item):
    """Обработчик меню «Запись области»."""
    if _tray_root:
        _tray_root.after(0, _tray_app.on_area_screen)


def _on_program(icon, item):
    """Обработчик меню «Запись программы»."""
    if _tray_root:
        _tray_root.after(0, _tray_app.on_program_screen)


def _on_stop(icon, item):
    """Обработчик меню «Стоп»: останавливает текущую запись."""
    if _tray_root:
        _tray_root.after(0, _tray_app.stop_current)


def _on_quit(icon, item):
    """Обработчик меню «Выход»: останавливает запись, закрывает окно и завершает иконку трея."""
    if _tray_app:
        _tray_app.stop_current()
    if _tray_root:
        _tray_root.after(10, _tray_root.destroy)
    icon.stop()


def setup(root_ref, appwin_ref):
    """Запускает иконку трея в отдельном фоновом потоке, сохраняя ссылки на окно и приложение."""
    global _tray_root, _tray_app
    _tray_root = root_ref
    _tray_app = appwin_ref
    import threading
    t = threading.Thread(target=create_tray_icon, args=(root_ref, appwin_ref), daemon=True)
    t.start()


def stop_tray_icon():
    """Пустая заглушка для остановки трея: поток живёт до завершения процесса."""
    pass
