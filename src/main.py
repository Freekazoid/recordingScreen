import multiprocessing as mp
import os
import sys
import tkinter as tk
import warnings

from PIL import Image, ImageTk

from app_paths import ensure_writable_base_dir, get_writable_base_dir
from gui_window import AppWindow
from logging_utils import clear_error_report

warnings.filterwarnings("ignore")
os.environ["TOKENIZERS_PARALLELISM"] = "false"
# НЕ отключаем TorchScript глобально: PYTORCH_JIT=0 ломает torch.jit.load,
# который использует silero-vad (PyAnnote) через utils_vad.init_jit_model —
# PyAnnote падает с "RecursiveScriptModule has no attribute _construct".
# NeMo на Python 3.14 / torch 2.11 импортируется и работает в eager-режиме
# без этого флага (см. nemo_compat для точечного отключения при NeMo).


def _get_base_dir():
    if getattr(sys, "frozen", False):
        return getattr(sys, "_MEIPASS", os.path.dirname(sys.executable))
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _cleanup_temp_dirs():
    bases = {os.getcwd(), get_writable_base_dir()}
    names = [".temp_postprocess", ".temp_segments", ".temp_audio"]
    for base in bases:
        for d in names:
            try:
                path = os.path.join(base, d)
                if os.path.isdir(path):
                    shutil.rmtree(path, ignore_errors=True)
            except Exception:
                pass


def main():
    # Дочерние процессы не должны видеть библиотеки бандла в LD_LIBRARY_PATH
    # (иначе системные утилиты вроде systemd-run падают по версиям символов).
    from proc_env import install_subprocess_guard

    install_subprocess_guard()

    if getattr(sys, "frozen", False):
        # Keep system dist-packages out of frozen imports (psutil/ABI clashes).
        meipass = getattr(sys, "_MEIPASS", None)
        cleaned = []
        for entry in sys.path:
            norm = os.path.normpath(entry or "")
            if meipass and norm.startswith(os.path.normpath(meipass)):
                cleaned.append(entry)
                continue
            if "dist-packages" in norm and ("/usr/lib/python" in norm or "/usr/local/lib/python" in norm):
                continue
            cleaned.append(entry)
        sys.path[:] = cleaned
        os.environ.setdefault("PYTHONNOUSERSITE", "1")

    clear_error_report("wayland")
    _cleanup_temp_dirs()
    ensure_writable_base_dir()

    # На Wayland нужен корректный app-id для порталов (диалоги доступа к
    # экрану). Если запущены «голым» python из терминала — перезапускаем
    # себя в именованном systemd-scope.
    from portal_identity import ensure_portal_identity

    ensure_portal_identity()

    root = tk.Tk(className="ScreenRecorder")

    base_dir = _get_base_dir()
    assets_dir = os.path.join(base_dir, "assets")

    root.tk.call("tk", "appname", "ScreenRecorder")
    root.wm_iconname("Screen Recorder")

    if sys.platform == "win32":
        ico_path = os.path.join(assets_dir, "icon.ico")
        if os.path.exists(ico_path):
            try:
                root.iconbitmap(ico_path)
            except Exception:
                pass
    else:
        png_path = os.path.join(assets_dir, "icon.png")
        if os.path.exists(png_path):
            try:
                icon_img = ImageTk.PhotoImage(Image.open(png_path))
                root.iconphoto(True, icon_img)
                root._icon_ref = icon_img
            except Exception:
                pass

    appwin = AppWindow(root)
    appwin.run()


if __name__ == "__main__":
    mp.freeze_support()
    main()
