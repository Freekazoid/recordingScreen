import json
import multiprocessing as mp
import os
import queue
import shutil
import sys
import time as time_module
import tkinter as tk
import webbrowser
from datetime import datetime
from tkinter import filedialog, ttk
from typing import Any

from app_paths import get_writable_base_dir, safe_timestamp

from PIL import Image, ImageTk

from area_screen import AreaScreenMode, select_screen_area, select_wayland_area
from audio_recorder import AudioRecorder
from audio_transcriber_service import AudioTranscriberService
from background_tasks import download_model_worker, postprocess_recording_worker
from ffmpeg_locator import ensure_ffmpeg, has_ffmpeg
from full_screen import FullScreenMode
from logging_utils import write_error_report
from model_manager import get_model_status, load_hf_token
from program_screen import ProgramScreenMode
from program_screen_win import WindowsProgramScreenMode
from settings_manager import load_settings, save_settings

_WAYLAND_BACKEND_IMPORT_ERROR = None
try:
    from program_screen_wayland import (
        WINDOW_SOURCE_TYPE,
        WaylandAreaScreenMode,
        WaylandFullScreenMode,
        WaylandProgramScreenMode,
        is_wayland_session,
        wayland_dependency_issue,
    )
except Exception as exc:  # pragma: no cover - optional dependency
    WaylandProgramScreenMode = None  # type: ignore
    WaylandFullScreenMode = None  # type: ignore
    WaylandAreaScreenMode = None  # type: ignore
    _WAYLAND_BACKEND_IMPORT_ERROR = str(exc)
    def is_wayland_session() -> bool:
        return bool(os.environ.get("WAYLAND_DISPLAY")) or os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland"
    def wayland_dependency_issue() -> str | None:
        return "модуль Wayland недоступен"
    WINDOW_SOURCE_TYPE = 2

if getattr(sys, "frozen", False):
    RESOURCE_BASE_DIR = getattr(sys, "_MEIPASS", os.path.dirname(sys.executable))
else:
    RESOURCE_BASE_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))



BG_COLOR = "#202020"
TEXT_COLOR = "#cccaca"
BTN_COLOR = "#353535"
ENTRY_BG = "#282828"
HINT_COLOR = "#e0b040"


def _show_tooltip(widget, text):
    """Всплывающая подсказка рядом с виджетом. Возвращает Toplevel."""
    tip = tk.Toplevel(widget)
    tip.wm_overrideredirect(True)
    lbl = tk.Label(
        tip,
        text=text,
        bg="#4a4230",
        fg=TEXT_COLOR,
        font=("Arial", 9),
        justify=tk.LEFT,
        wraplength=340,
        padx=10,
        pady=6,
        relief="solid",
        bd=1,
    )
    lbl.pack()
    tip.update_idletasks()
    x = widget.winfo_rootx() + widget.winfo_width() + 12
    y = widget.winfo_rooty() + (widget.winfo_height() - tip.winfo_reqheight()) // 2
    screen_w = widget.winfo_screenwidth()
    if x + tip.winfo_reqwidth() > screen_w - 8:
        x = max(8, widget.winfo_rootx() - tip.winfo_reqwidth() - 12)
    tip.wm_geometry(f"+{max(8, x)}+{max(8, y)}")
    return tip


def _attach_tooltip(widget, text):
    """Показывать подсказку при наведении, скрывать при уходе курсора."""
    state = {"tip": None}

    def _enter(_event):
        if state["tip"] is None or not state["tip"].winfo_exists():
            state["tip"] = _show_tooltip(widget, text)

    def _leave(_event):
        if state["tip"] is not None:
            try:
                state["tip"].destroy()
            except tk.TclError:
                pass
            state["tip"] = None

    widget.bind("<Enter>", _enter)
    widget.bind("<Leave>", _leave)
    widget.bind("<ButtonPress>", _leave)


def _make_info_icon(parent, text):
    """Значок «ⓘ», показывающий подсказку при наведении курсора."""
    icon = tk.Label(
        parent,
        text="ⓘ",
        bg=parent["bg"] if isinstance(parent["bg"], str) else BG_COLOR,
        fg=HINT_COLOR,
        font=("Arial", 11, "bold"),
        cursor="hand2",
    )
    _attach_tooltip(icon, text)
    return icon
AUDIO_OUTPUT = "audio.wav"
VIDEO_OUTPUT = "output.mkv"
MODEL_STATUS_COLORS = {
    "ready": "#46B86F",
    "not_ready": "#EE5454",
    "downloading": "#DBB63B"
}
MODE_NAMES = {
    "fullscreen": "Весь экран",
    "area": "Область экрана",
    "program": "Программа",
    "audio": "Только аудио"
}
TRANSCRIPTION_ENGINES = ["whisperx", "vosk"]
DIARIZATION_METHODS = ["none", "diarize", "nemo"]
# Вычислительное устройство для транскрибации: auto -> GPU если есть, иначе CPU.
COMPUTE_DEVICES = ["auto", "gpu", "cpu"]
COMPUTE_DEVICE_LABELS = {"auto": "Авто", "gpu": "GPU", "cpu": "CPU"}
OUTPUT_FORMATS = ["mp4", "mkv", "webm", "avi", "mov", "flv"]
VIDEO_QUALITY_LABEL_TO_CRF = {
    "Отличное (CRF 18)": 18,
    "Хорошее (CRF 23)": 23,
    "Среднее (CRF 28)": 28,
    "Сильное сжатие (CRF 35)": 35,
}
AUDIO_TRACK_LABEL_TO_MODE = {
    "Как есть (без пережатия)": "copy",
    "Сжать (AAC 128k)": "aac128",
    "Без звука": "none",
}

def ask_hf_token_gui(root) -> str | None:
    """Диалоговое окно ввода токена HuggingFace. Возвращает введённый токен или None при отмене."""
    import webbrowser
    from tkinter import CENTER, Button, Entry, Label, StringVar, Toplevel

    dlg = Toplevel(root)
    dlg.title("HuggingFace Token")
    dlg.configure(bg=BG_COLOR)
    dlg.resizable(False, False)
    width, height = 500, 220
    x = root.winfo_rootx() + max(0, (root.winfo_width() - width) // 2)
    y = root.winfo_rooty() + max(0, (root.winfo_height() - height) // 2)
    dlg.geometry(f"{width}x{height}+{x}+{y}")
    dlg.update_idletasks()
    try:
        dlg.wait_visibility()
        dlg.grab_set()
    except Exception:
        pass

    Label(
        dlg,
        text="Вставьте HF_TOKEN. Получить можно на странице:\nhttps://huggingface.co/settings/tokens",
        bg=BG_COLOR, fg=TEXT_COLOR,
        font=('Arial', 11),
        justify=CENTER,
        wraplength=460
    ).pack(padx=12, pady=(17, 8))

    Button(
        dlg,
        text="Открыть страницу токенов",
        command=lambda: webbrowser.open("https://huggingface.co/settings/tokens"),
        font=('Arial', 10),
        bg=BTN_COLOR, fg=TEXT_COLOR,
        activebackground=ENTRY_BG,
        relief="raised",
        bd=1
    ).pack(pady=(0, 8))

    var = StringVar()
    entry = Entry(
        dlg, textvariable=var,
        font=('Arial', 13),
        bg=ENTRY_BG, fg=TEXT_COLOR, insertbackground=TEXT_COLOR,
        highlightthickness=1, relief="solid", bd=1
    )
    entry.pack(padx=16, pady=2, ipadx=8, ipady=8, fill='x')
    entry.focus()


    result: dict[str, str | None] = {"token": None}
    def on_ok():
        token = var.get().strip()
        if token:
            result["token"] = token
            dlg.destroy()

    def on_cancel():
        dlg.destroy()

    btn = Button(
        dlg, text="OK", command=on_ok,
        font=('Arial', 11),
        bg=BTN_COLOR, fg=TEXT_COLOR,
        activebackground=ENTRY_BG,
        relief="raised",
        bd=1
    )
    btn.pack(pady=(10,8), ipadx=10)

    dlg.protocol("WM_DELETE_WINDOW", on_cancel)
    dlg.bind('<Return>', lambda _: on_ok())
    dlg.wait_window()
    return result["token"]

def _get_git_version() -> str:
    """Возвращает короткий хеш последнего коммита Git."""
    try:
        import subprocess
        result = subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"],
            capture_output=True, text=True, timeout=5
        )
        return result.stdout.strip() if result.returncode == 0 else "unknown"
    except Exception:
        return "unknown"


def _get_app_version() -> str:
    """Версия сборки для окна «О программе».

    В AppImage — та же, что в имени артефакта (запекается build_AppImage.sh
    в src/_build_version.py при сборке, включая CI с его APP_VERSION).
    В dev-режиме — короткий хеш коммита.
    """
    if getattr(sys, "frozen", False):
        try:
            from _build_version import APP_VERSION

            return str(APP_VERSION)
        except Exception:
            pass
    return _get_git_version()

def _check_disk_space(min_gb: float = 1.0) -> bool:
    """Проверяет, есть ли минимум min_gb свободного места на диске."""
    import shutil
    try:
        usage = shutil.disk_usage(".")
        free_gb = usage.free / (1024 ** 3)
        if free_gb < min_gb:
            print(f"Предупреждение: мало места на диске ({free_gb:.1f} ГБ свободно, нужно {min_gb} ГБ)")
            return False
        return True
    except Exception:
        return True


class AppWindow:
    """Главное окно приложения-рекордера экрана на Tkinter: управляет записью, настройками и постобработкой."""
    def __init__(self, root):
        """Инициализация главного окна и всех виджетов интерфейса, загрузка настроек и запуск фоновых задач."""
        self.root = root
        self.audio_recorder = AudioRecorder(filename=AUDIO_OUTPUT)
        self.current_mode: dict[str, str | None] = {"mode": None}
        self.current_instance: dict[str, Any | None] = {"obj": None}
        self.video_start_time = None
        self.mp_context = mp.get_context("spawn")
        self.model_download_jobs = {}
        self.postprocess_jobs = {}
        self.runtime_status_override = None
        self._spinner_frames = ["|", "/", "-", "\\"]
        self._spinner_index = 0

        settings = load_settings()

        engine = str(settings.get("transcription_engine", "") or "").strip()
        if engine not in TRANSCRIPTION_ENGINES:
            engine = "whisperx"
        self.transcription_engine = tk.StringVar(value=engine)
        diar_method = str(settings.get("diarization_method", "diarize") or "diarize").strip()
        if diar_method not in DIARIZATION_METHODS:
            diar_method = "diarize"
        self.diarization_method = tk.StringVar(value=diar_method)
        output_format = str(settings.get("output_format", "mp4")).lower().strip()
        if output_format not in OUTPUT_FORMATS:
            output_format = "mp4"
        self.output_format = tk.StringVar(value=output_format)

        video_quality = str(settings.get("video_quality", "Хорошее (CRF 23)")).strip()
        if video_quality not in VIDEO_QUALITY_LABEL_TO_CRF:
            video_quality = "Хорошее (CRF 23)"
        self.video_quality = tk.StringVar(value=video_quality)

        audio_track_mode = str(settings.get("audio_track_mode", "Как есть (без пережатия)")).strip()
        if audio_track_mode not in AUDIO_TRACK_LABEL_TO_MODE:
            audio_track_mode = "Как есть (без пережатия)"
        self.audio_track_mode = tk.StringVar(value=audio_track_mode)

        compute_device = str(settings.get("compute_device", "auto")).strip().lower()
        if compute_device not in COMPUTE_DEVICES:
            compute_device = "auto"
        self.compute_device = tk.StringVar(value=compute_device)

        default_data_dir = get_writable_base_dir()
        models_dir = str(settings.get("models_dir", "")).strip() or default_data_dir
        self.models_dir_var = tk.StringVar(value=os.path.abspath(models_dir))
        output_dir = str(settings.get("output_dir", "")).strip() or default_data_dir
        self.output_dir_var = tk.StringVar(value=os.path.abspath(output_dir))

        # Единый реестр «настройка -> переменная»: единственная точка,
        # по которой собираются и сохраняются все настройки.
        # Новая настройка = добавить var + одну строку здесь.
        self._setting_vars: dict[str, tk.Variable] = {
            "transcription_engine": self.transcription_engine,
            "diarization_method": self.diarization_method,
            "output_format": self.output_format,
            "video_quality": self.video_quality,
            "audio_track_mode": self.audio_track_mode,
            "compute_device": self.compute_device,
            "models_dir": self.models_dir_var,
            "output_dir": self.output_dir_var,
        }

        if models_dir:
            from model_manager import resolve_models_path as _resolve_models_path
            from background_tasks import apply_models_root
            models_path = _resolve_models_path(models_dir)
            os.makedirs(models_path, exist_ok=True)
            apply_models_root(models_path)

        os.makedirs(os.path.abspath(output_dir), exist_ok=True)

        self.root.configure(bg=BG_COLOR)
        self.root.title("Рекордер экрана")
        self.root.geometry("380x600")
        self.root.resizable(False, False)

        style = ttk.Style()
        style.theme_use('clam')
        style.configure("TNotebook", background=BG_COLOR, borderwidth=0)
        style.configure("TNotebook", tabmargins=[0, 0, 0, 0])
        style.configure("TNotebook.Tab",
            background=ENTRY_BG,
            foreground=TEXT_COLOR,
            padding=[8, 8],
            width=13,
            borderwidth=0,
            relief="flat"
        )
        style.map("TNotebook.Tab",
            background=[("selected", BTN_COLOR), ("!selected", ENTRY_BG)],
            foreground=[("selected", "#FFFFFF"), ("!selected", TEXT_COLOR)],
            padding=[("selected", [8, 8]), ("!selected", [8, 8])],
            relief=[("selected", "flat"), ("!selected", "flat")],
            borderwidth=[("selected", 0), ("!selected", 0)]
        )
        style.configure("TFrame", background=BG_COLOR)
        style.configure("Dark.TCombobox",
            fieldbackground=BTN_COLOR,
            background=BTN_COLOR,
            foreground=TEXT_COLOR,
            arrowcolor=TEXT_COLOR,
            bordercolor=BTN_COLOR,
            lightcolor=BTN_COLOR,
            darkcolor=BTN_COLOR,
            padding=3
        )
        style.map("Dark.TCombobox",
            fieldbackground=[("readonly", BTN_COLOR), ("!readonly", ENTRY_BG)],
            foreground=[("readonly", TEXT_COLOR), ("active", TEXT_COLOR)],
            selectbackground=[("readonly", BTN_COLOR), ("!readonly", ENTRY_BG)],
            selectforeground=[("readonly", TEXT_COLOR), ("!readonly", TEXT_COLOR)],
            background=[("active", BTN_COLOR), ("pressed", BTN_COLOR)],
        )
        style.configure("Dark.Vertical.TScrollbar",
            width=8,
            background=BTN_COLOR,
            troughcolor=BG_COLOR,
            bordercolor=BG_COLOR,
            arrowcolor=TEXT_COLOR,
            relief="flat",
        )

        self.fullscreen_img = self.load_icon("./assets/fullscreen.png")
        self.area_img = self.load_icon("./assets/area.png")
        self.program_img = self.load_icon("./assets/program.png")
        self.empty_img = self.load_icon("./assets/empty.png")

        self.notebook = ttk.Notebook(root)
        self.notebook.pack(fill=tk.BOTH, expand=True, padx=5, pady=5)

        self.tab_record = ttk.Frame(self.notebook, style="Dark.TFrame")
        self.tab_settings = ttk.Frame(self.notebook, style="Dark.TFrame")
        self.tab_about = ttk.Frame(self.notebook, style="Dark.TFrame")

        for frame in [self.tab_record, self.tab_settings, self.tab_about]:
            tk.Frame(frame, bg=BG_COLOR).place(relwidth=1, relheight=1)

        self.notebook.add(self.tab_record, text="Запись")
        self.notebook.add(self.tab_settings, text="Настройки")
        self.notebook.add(self.tab_about, text="О авторе")

        about_wrap = 320
        about_frame = tk.Frame(self.tab_about, bg=BG_COLOR)
        about_frame.pack(fill=tk.BOTH, expand=True, padx=14, pady=14)

        tk.Label(
            about_frame,
            text="О авторе",
            bg=BG_COLOR,
            fg=TEXT_COLOR,
            font=("Arial", 12, "bold"),
            anchor="w",
        ).pack(fill=tk.X, pady=(0, 8))

        # --- Загрузка и отображение картинки и ссылок в один ряд ---

        # Загрузка картинки проекта (если файл существует, иначе не вызовет ошибки при отладке, если убрать проверку)
        try:
            self.about_project_img = self.load_icon("./assets/ava100x100.png", size=(100, 100))
        except Exception:
            self.about_project_img = None

        # Рамка для горизонтального расположения (картинка слева, ссылки справа)
        top_row_frame = tk.Frame(about_frame, bg=BG_COLOR)
        top_row_frame.pack(fill=tk.X, pady=(0, 10))

        if self.about_project_img:
            # Картинка
            tk.Label(top_row_frame, image=self.about_project_img, bg=BG_COLOR).pack(side=tk.LEFT, padx=(0, 15))

        # Контейнер для ссылок справа от картинки
        links_frame = tk.Frame(top_row_frame, bg=BG_COLOR)
        links_frame.pack(side=tk.LEFT, fill=tk.X, expand=True)

        tg_link = tk.Label(
            links_frame,
            text="Автор (Telegram)",
            bg=BG_COLOR,
            fg="#8CC8FF",
            font=("Arial", 10, "underline"),
            cursor="hand2",
            anchor="w",
            justify="left",
            wraplength=250, # Ограничиваем ширину ссылки, чтобы она влезла рядом с картинкой
        )
        tg_link.pack(fill=tk.X, pady=(0, 2))
        tg_link.bind("<Button-1>", lambda _e: webbrowser.open("https://t.me/Ifreekazoid"))

        project_link = tk.Label(
            links_frame,
            text="Проект (github.com)",
            bg=BG_COLOR,
            fg="#8CC8FF",
            font=("Arial", 10, "underline"),
            cursor="hand2",
            anchor="w",
            justify="left",
            wraplength=250,
        )
        project_link.pack(fill=tk.X)
        project_link.bind(
            "<Button-1>",
            lambda _e: webbrowser.open("https://github.com/Freekazoid/recordingScreen"),
        )
        about_text = (
            "Утилита для автоматизации записи и анализа рабочих встреч и дейликов. "
            "Позволяет захватывать экран и аудио, а затем с помощью ИИ-моделей (WhisperX, Vosk, NeMo) "
            "автоматически транскрибировать речь, разделять реплики по спикерам и формировать готовые конспекты.\n\n"
            "Главная цель — фиксировать принятые решения, быстро извлекать задачи из диалога и подготавливать "
            "структурированные материалы для трекеров и командной работы. Вся обработка речи выполняется "
            "на стороне клиента с использованием открытых нейросетевых моделей."
        )

        tk.Label(
            about_frame,
            text=about_text,
            bg=BG_COLOR,
            fg=TEXT_COLOR,
            font=("Arial", 10),
            justify="left",
            anchor="nw",
            wraplength=about_wrap,
        ).pack(fill=tk.X, pady=(12, 0))

        tk.Label(
            about_frame,
            text="Автор (Telegram): https://t.me/Ifreekazoid",
            bg=BG_COLOR,
            fg="#8CC8FF",
            font=("Arial", 10, "underline"),
            cursor="hand2",
            anchor="w",
            justify="left",
            wraplength=about_wrap,
        )

        tk.Label(
            about_frame,
            text=f"Версия сборки: {_get_app_version()}",
            bg=BG_COLOR,
            fg=TEXT_COLOR,
            font=("Arial", 10, "italic"),
            anchor="w",
        ).pack(side=tk.BOTTOM, fill=tk.X, pady=(10, 0))

        model_status_frame = tk.Frame(self.tab_record, bg=BG_COLOR)
        model_status_frame.pack(side=tk.TOP, fill=tk.X, pady=5)
        self.model_status_color = tk.StringVar(value=MODEL_STATUS_COLORS["not_ready"])
        self.model_status_text = tk.StringVar(value="Модели не скачены")
        self.recording_status_text = tk.StringVar(value="Модели не скачены")
        self.downloading_flag = {"running": False}
        self.model_status_canvas = tk.Canvas(model_status_frame, width=32, height=24, bg=BG_COLOR, bd=0, highlightthickness=0)
        self.oval = self.model_status_canvas.create_oval(5, 5, 23, 23, fill=MODEL_STATUS_COLORS["not_ready"])
        self.model_status_canvas.pack(side=tk.LEFT, padx=(10,2))
        model_status_label = tk.Label(
            model_status_frame, textvariable=self.recording_status_text,
            bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 11), anchor="w"
        )
        model_status_label.pack(side=tk.LEFT, padx=(7,0), fill="x", expand=True)

        self.log_text = tk.Text(self.tab_record, height=4, bg=BG_COLOR, fg="#BBBB88", font=("Courier New", 10), state='disabled', wrap='word')
        self.log_text.pack(side=tk.BOTTOM, fill=tk.X, padx=10, pady=(12, 8))
        self.global_log_lines = []

        self.button_full = tk.Button(
            self.tab_record, text="Весь экран",
            image=self.fullscreen_img, compound="left",
            bg=BTN_COLOR, fg=TEXT_COLOR,
            activebackground=ENTRY_BG, activeforeground=TEXT_COLOR,
            font=("Arial", 14),
            width=200, height=40,
            command=self.on_full_screen,
            bd=0, relief="flat", highlightthickness=0, takefocus=0
        )
        self.button_area = tk.Button(
            self.tab_record, text="Область экрана",
            image=self.area_img, compound="left",
            bg=BTN_COLOR, fg=TEXT_COLOR,
            activebackground=ENTRY_BG, activeforeground=TEXT_COLOR,
            font=("Arial", 14),
            width=200, height=40,
            command=self.on_area_screen,
            bd=0, relief="flat", highlightthickness=0, takefocus=0
        )
        self.button_program = tk.Button(
            self.tab_record, text="Программа",
            image=self.program_img, compound="left",
            bg=BTN_COLOR, fg=TEXT_COLOR,
            activebackground=ENTRY_BG, activeforeground=TEXT_COLOR,
            font=("Arial", 14),
            width=200, height=40,
            command=self.on_program_screen,
            bd=0, relief="flat", highlightthickness=0, takefocus=0
        )
        self.button_audio = tk.Button(
            self.tab_record, text="Только аудио",
            image=self.empty_img, compound="left",
            bg=BTN_COLOR, fg=TEXT_COLOR,
            activebackground=ENTRY_BG, activeforeground=TEXT_COLOR,
            font=("Arial", 14),
            width=200, height=40,
            command=self.on_audio_only,
            bd=0, relief="flat", highlightthickness=0, takefocus=0
        )
        self.button_full.pack(pady=8)
        self.button_area.pack(pady=8)
        self.button_program.pack(pady=8)
        self.button_audio.pack(pady=8)

        export_frame = tk.LabelFrame(
            self.tab_record,
            text="Формат и сжатие",
            bg=BG_COLOR,
            fg=TEXT_COLOR,
            font=("Arial", 10, "bold"),
        )
        export_frame.pack(fill=tk.X, padx=10, pady=(4, 8))

        tk.Label(export_frame, text="Формат файла:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 10)).grid(
            row=0, column=0, sticky="w", padx=8, pady=(8, 4)
        )
        format_menu = ttk.Combobox(
            export_frame,
            textvariable=self.output_format,
            values=OUTPUT_FORMATS,
            width=24,
            style="Dark.TCombobox",
            state="readonly",
            font=("Arial", 10),
        )
        format_menu.grid(row=0, column=1, sticky="ew", padx=(8, 10), pady=(8, 4))
        _attach_tooltip(
            format_menu,
            "Формат (контейнер) итогового видеофайла.\n\n"
            "MP4 — самый совместимый, открывается везде.\n"
            "MKV — надёжный, подходит для длинных записей.\n"
            "WebM, AVI, MOV, FLV — для особых случаев совместимости.",
        )

        tk.Label(export_frame, text="Качество видео:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 10)).grid(
            row=1, column=0, sticky="w", padx=8, pady=4
        )
        quality_menu = ttk.Combobox(
            export_frame,
            textvariable=self.video_quality,
            values=list(VIDEO_QUALITY_LABEL_TO_CRF.keys()),
            width=24,
            style="Dark.TCombobox",
            state="readonly",
            font=("Arial", 10),
        )
        quality_menu.grid(row=1, column=1, sticky="ew", padx=(8, 10), pady=4)
        _attach_tooltip(
            quality_menu,
            "Качество видео. CRF — чем меньше число, тем выше качество "
            "и крупнее файл.\n\n"
            "• Отличное (CRF 18) — почти без потерь\n"
            "• Хорошее (CRF 23) — баланс качества и размера\n"
            "• Среднее (CRF 28) — заметная разница, файл меньше\n"
            "• Сильное сжатие (CRF 35) — минимальный размер файла",
        )

        tk.Label(export_frame, text="Звуковая дорожка:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 10)).grid(
            row=2, column=0, sticky="w", padx=8, pady=(4, 8)
        )
        audio_menu = ttk.Combobox(
            export_frame,
            textvariable=self.audio_track_mode,
            values=list(AUDIO_TRACK_LABEL_TO_MODE.keys()),
            width=24,
            style="Dark.TCombobox",
            state="readonly",
            font=("Arial", 10),
        )
        audio_menu.grid(row=2, column=1, sticky="ew", padx=(8, 10), pady=(4, 8))
        _attach_tooltip(
            audio_menu,
            "Обработка звука в итоговом файле.\n\n"
            "• Как есть (без пережатия) — звук сохраняется как записан, "
            "без перекодирования (быстро)\n"
            "• Сжать (AAC 128k) — файл компактнее, совместимо везде\n"
            "• Без звука — в итоговом файле останется только видео",
        )
        export_frame.columnconfigure(1, weight=1)

        self.settings_canvas = tk.Canvas(self.tab_settings, bg=BG_COLOR, bd=0, highlightthickness=0)
        self.settings_scrollbar = tk.Scrollbar(
            self.tab_settings, orient=tk.VERTICAL,
            command=self.settings_canvas.yview,
            width=4, bg=BTN_COLOR, troughcolor=BG_COLOR,
            bd=0, relief="flat", highlightthickness=0,
            activebackground=BTN_COLOR, borderwidth=0
        )
        self.settings_canvas.configure(yscrollcommand=self.settings_scrollbar.set)
        self.settings_scrollbar.pack(side=tk.RIGHT, fill=tk.Y)
        self.settings_canvas.pack(side=tk.LEFT, fill=tk.BOTH, expand=True)

        settings_inner = tk.Frame(self.settings_canvas, bg=BG_COLOR)
        self.settings_canvas_window = self.settings_canvas.create_window((0, 0), window=settings_inner, anchor="nw")
        settings_inner.bind("<Configure>", lambda _e: self.settings_canvas.configure(scrollregion=self.settings_canvas.bbox("all")))
        self.settings_canvas.bind("<Configure>", lambda e: self.settings_canvas.itemconfigure(self.settings_canvas_window, width=e.width))
        self.root.bind_all("<MouseWheel>", self._on_settings_mousewheel, add="+")
        self.root.bind_all("<Button-4>", self._on_settings_mousewheel, add="+")
        self.root.bind_all("<Button-5>", self._on_settings_mousewheel, add="+")

        # Секция статуса моделей
        model_frame = tk.LabelFrame(settings_inner, text="Модели", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 11, "bold"))
        model_frame.pack(fill=tk.X, pady=5)

        self.model_bars = {}
        self.model_buttons = {}
        self._model_titles = {
            "whisperx": "WhisperX (large-v3-turbo)",
            "vosk": "Vosk",
            "pyannote": "PyAnnote",
            "nemo": "NeMo Sortformer",
        }

        def make_model_row(parent, title, description, model_name):
            row = tk.Frame(parent, bg=BG_COLOR)
            row.pack(fill=tk.X, padx=5, pady=2)

            top = tk.Frame(row, bg=BG_COLOR)
            top.pack(fill=tk.X)

            # Только название модели; подробности — под значком «ⓘ»
            label = tk.Label(
                top, text=title,
                bg=BG_COLOR, fg=TEXT_COLOR,
                font=("Arial", 10), anchor="w",
            )
            label.pack(side=tk.LEFT)

            _make_info_icon(top, description).pack(side=tk.LEFT, padx=(5, 0))

            btn_holder = tk.Frame(top, bg=BG_COLOR)
            btn_holder.pack(side=tk.RIGHT)

            btn_dl = tk.Button(
                btn_holder, text="Скачать", width=10,
                bg=BTN_COLOR, fg=TEXT_COLOR,
                activebackground=ENTRY_BG, activeforeground=TEXT_COLOR,
                font=("Arial", 9), bd=0, relief="flat",
                highlightthickness=0, takefocus=0,
                command=lambda: self._download_model(model_name),
            )
            btn_dl.pack(side=tk.LEFT, padx=(4, 2))

            btn_del = tk.Button(
                btn_holder, text="Удалить", width=8,
                bg=BTN_COLOR, fg=TEXT_COLOR,
                activebackground=ENTRY_BG, activeforeground=TEXT_COLOR,
                font=("Arial", 9), bd=0, relief="flat",
                highlightthickness=0, takefocus=0,
                state="disabled",
                command=lambda: self._delete_model(model_name),
            )
            btn_del.pack(side=tk.LEFT)

            bar = ttk.Progressbar(row, maximum=100, mode="determinate")
            bar.pack(fill=tk.X, pady=(2, 0))

            self.model_bars[model_name] = bar
            self.model_buttons[model_name] = {"download": btn_dl, "delete": btn_del}

        make_model_row(
            model_frame,
            "WhisperX (large-v3-turbo)",
            "Транскрибация речи в текст.\n\n"
            "faster-whisper large-v3-turbo (на базе Whisper от OpenAI): "
            "быстрое и точное распознавание, хорошо работает с русским языком. "
            "HF-токен не нужен.\n\n"
            "Рекомендуется, если есть видеокарта (GPU).",
            "whisperx",
        )
        make_model_row(
            model_frame,
            "Vosk",
            "Транскрибация речи в текст.\n\n"
            "Лёгкая офлайн-модель Vosk (русский язык): работает быстро даже "
            "без GPU, но точность ниже, чем у WhisperX.\n\n"
            "Вариант для слабых компьютеров.",
            "vosk",
        )
        make_model_row(
            model_frame,
            "PyAnnote",
            "Диаризация: определяет, кто и когда говорил, и размечает текст "
            "по спикерам («Спикер 1», «Спикер 2»…).\n\n"
            "PyAnnote speaker-diarization-3.1 (HuggingFace). Для скачивания "
            "нужен бесплатный токен HF_TOKEN — будет запрошен при загрузке.",
            "pyannote",
        )
        make_model_row(
            model_frame,
            "NeMo Sortformer",
            "Диаризация: определяет, кто и когда говорил — до 4 спикеров "
            "одновременно.\n\n"
            "NeMo Sortformer (NVIDIA). HF-токен не нужен.\n\n"
            "Альтернатива PyAnnote.",
            "nemo",
        )

        # Разделитель
        tk.Frame(settings_inner, height=2, bg=BTN_COLOR).pack(fill=tk.X, pady=10)

        trans_label = tk.Label(settings_inner, text="Транскрипция:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 12))
        trans_label.pack(anchor="w", pady=(10, 5))

        trans_menu = ttk.Combobox(settings_inner, textvariable=self.transcription_engine,
                                   values=TRANSCRIPTION_ENGINES, width=15,
                                   style="Dark.TCombobox", state="readonly", font=("Arial", 11))
        trans_menu.pack(anchor="w", padx=10)

        diar_label = tk.Label(settings_inner, text="Спикеры:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 12))
        diar_label.pack(anchor="w", pady=(10, 5))

        diar_menu = ttk.Combobox(settings_inner, textvariable=self.diarization_method,
                                   values=DIARIZATION_METHODS, width=15,
                                   style="Dark.TCombobox", state="readonly", font=("Arial", 11))
        diar_menu.pack(anchor="w", padx=10)

        dev_label = tk.Label(settings_inner, text="Вычислительное устройство:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 12))
        dev_label.pack(anchor="w", pady=(10, 5))

        dev_menu = ttk.Combobox(settings_inner, textvariable=self.compute_device,
                                values=COMPUTE_DEVICES, width=15,
                                style="Dark.TCombobox", state="readonly", font=("Arial", 11))
        dev_menu.pack(anchor="w", padx=10)
        _attach_tooltip(
            dev_menu,
            "Устройство для распознавания речи:\n"
            "• Авто — GPU, если он доступен, иначе CPU.\n"
            "• GPU — использовать видеокарту (нужен CUDA-torch).\n"
            "• CPU — только процессор (медленнее, но работает везде).",
        )

        # Разделитель
        tk.Frame(settings_inner, height=2, bg=BTN_COLOR).pack(fill=tk.X, pady=10)

        self.enable_transcription = tk.BooleanVar(value=settings.get("enable_transcription", True))
        self.enable_diarization = tk.BooleanVar(value=settings.get("enable_diarization", True))
        self.include_timecodes = tk.BooleanVar(value=settings.get("include_timecodes", False))
        self.debug_mode = tk.BooleanVar(value=settings.get("debug_mode", False))
        self.expected_speakers = tk.StringVar(value=str(settings.get("expected_speakers", "")))
        self.min_speakers = tk.StringVar(value=str(settings.get("min_speakers", "")))
        self.max_speakers = tk.StringVar(value=str(settings.get("max_speakers", "")))

        self._setting_vars.update({
            "enable_transcription": self.enable_transcription,
            "enable_diarization": self.enable_diarization,
            "include_timecodes": self.include_timecodes,
            "debug_mode": self.debug_mode,
            "expected_speakers": self.expected_speakers,
            "min_speakers": self.min_speakers,
            "max_speakers": self.max_speakers,
        })

        for var in self._setting_vars.values():
            var.trace_add("write", self._on_setting_change)
        self.enable_diarization.trace_add("write", self._on_diarization_toggle)

        cb_trans = tk.Checkbutton(
            settings_inner, text="Включить распознавание речи",
            variable=self.enable_transcription,
            bg=BG_COLOR, fg=TEXT_COLOR, selectcolor=BTN_COLOR,
            font=("Arial", 11),bd=0, relief="flat"
        )
        cb_trans.pack(anchor="w", pady=5)
        _attach_tooltip(
            cb_trans,
            "После остановки записи речь автоматически расшифровывается в текст.\n\n"
            "Используется модель транскрибации, выбранная выше (WhisperX или Vosk).",
        )

        cb_diar = tk.Checkbutton(
            settings_inner, text="Распознавание по спикерам",
            variable=self.enable_diarization,
            bg=BG_COLOR, fg=TEXT_COLOR, selectcolor=BTN_COLOR,
            font=("Arial", 11),bd=0, relief="flat"
        )
        cb_diar.pack(anchor="w", pady=5)
        _attach_tooltip(
            cb_diar,
            "Текст разделяется по участникам разговора («Спикер 1», «Спикер 2»…).\n\n"
            "Нужна скачанная модель диаризации (PyAnnote или NeMo Sortformer) — "
            "см. блок «Модели» выше.",
        )

        cb_timecodes = tk.Checkbutton(
            settings_inner, text="Временные метки в тексте",
            variable=self.include_timecodes,
            bg=BG_COLOR, fg=TEXT_COLOR, selectcolor=BTN_COLOR,
            font=("Arial", 11), bd=0, relief="flat"
        )
        cb_timecodes.pack(anchor="w", pady=5)
        _attach_tooltip(
            cb_timecodes,
            "Перед каждой репликой указывается время её начала, например:\n"
            "[00:01:23] Спикер 1: Добрый день.",
        )

        self.speaker_limits_frame = tk.Frame(settings_inner, bg=BG_COLOR)
        tk.Label(
            self.speaker_limits_frame,
            text="Ожидаемые спикеры:",
            bg=BG_COLOR,
            fg=TEXT_COLOR,
            font=("Arial", 10),
        ).grid(row=0, column=0, sticky="w", pady=2)
        tk.Entry(
            self.speaker_limits_frame,
            textvariable=self.expected_speakers,
            width=8,
            bg=ENTRY_BG,
            fg=TEXT_COLOR,
            insertbackground=TEXT_COLOR,
            relief="flat",
        ).grid(row=0, column=1, sticky="w", padx=(6, 0), pady=2)
        _make_info_icon(
            self.speaker_limits_frame,
            "Важно: если указать 1, весь текст будет помечен как "
            "«Спикер 1», даже если говорили несколько человек.\n\n"
            "Оставьте поле пустым для автоопределения числа спикеров "
            "или укажите точное число ≥ 2 (либо диапазон Min–Max).",
        ).grid(row=0, column=2, sticky="w", padx=(4, 0), pady=2)

        tk.Label(
            self.speaker_limits_frame,
            text="Min:",
            bg=BG_COLOR,
            fg=TEXT_COLOR,
            font=("Arial", 10),
        ).grid(row=1, column=0, sticky="w", pady=2)
        tk.Entry(
            self.speaker_limits_frame,
            textvariable=self.min_speakers,
            width=8,
            bg=ENTRY_BG,
            fg=TEXT_COLOR,
            insertbackground=TEXT_COLOR,
            relief="flat",
        ).grid(row=1, column=1, sticky="w", padx=(6, 0), pady=2)

        tk.Label(
            self.speaker_limits_frame,
            text="Max:",
            bg=BG_COLOR,
            fg=TEXT_COLOR,
            font=("Arial", 10),
        ).grid(row=2, column=0, sticky="w", pady=2)
        tk.Entry(
            self.speaker_limits_frame,
            textvariable=self.max_speakers,
            width=8,
            bg=ENTRY_BG,
            fg=TEXT_COLOR,
            insertbackground=TEXT_COLOR,
            relief="flat",
        ).grid(row=2, column=1, sticky="w", padx=(6, 0), pady=2)

        # Секция директорий
        tk.Frame(settings_inner, height=2, bg=BTN_COLOR).pack(fill=tk.X, pady=10)

        paths_frame = tk.LabelFrame(settings_inner, text="Директории", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 11, "bold"))
        paths_frame.pack(fill=tk.X, pady=5)

        # Директория моделей
        models_dir_label = tk.Label(paths_frame, text="Папка моделей:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 10))
        models_dir_label.pack(anchor="w", padx=5, pady=(8, 2))
        models_path_frame = tk.Frame(paths_frame, bg=BG_COLOR)
        models_path_frame.pack(fill=tk.X, padx=5, pady=(0, 5))
        models_dir_entry = tk.Entry(
            models_path_frame, textvariable=self.models_dir_var,
            bg=ENTRY_BG, fg=TEXT_COLOR, insertbackground=TEXT_COLOR,
            relief="flat", font=("Arial", 9)
        )
        models_dir_entry.pack(side=tk.LEFT, fill=tk.X, expand=True, padx=(0, 5))
        _models_dir_hint = (
            "Каталог для хранения скачанных AI-моделей.\n\n"
            "Оставьте пустым — используется каталог данных приложения "
            "(~/.local/share/ScreenRecorder/models)."
        )
        _attach_tooltip(models_dir_label, _models_dir_hint)
        _attach_tooltip(models_dir_entry, _models_dir_hint)
        tk.Button(
            models_path_frame, text="Обзор", bg=BTN_COLOR, fg=TEXT_COLOR,
            relief="flat", font=("Arial", 9),
            command=lambda: self._browse_dir(self.models_dir_var)
        ).pack(side=tk.RIGHT)

        # Директория результатов
        output_dir_label = tk.Label(paths_frame, text="Папка для результатов:", bg=BG_COLOR, fg=TEXT_COLOR, font=("Arial", 10))
        output_dir_label.pack(anchor="w", padx=5, pady=(8, 2))
        output_path_frame = tk.Frame(paths_frame, bg=BG_COLOR)
        output_path_frame.pack(fill=tk.X, padx=5, pady=(0, 8))
        output_dir_entry = tk.Entry(
            output_path_frame, textvariable=self.output_dir_var,
            bg=ENTRY_BG, fg=TEXT_COLOR, insertbackground=TEXT_COLOR,
            relief="flat", font=("Arial", 9)
        )
        output_dir_entry.pack(side=tk.LEFT, fill=tk.X, expand=True, padx=(0, 5))
        _output_dir_hint = (
            "Куда сохранять готовые файлы: видеозаписи, аудио и транскрипции.\n\n"
            "Пусто — файлы сохраняются в каталог запуска приложения."
        )
        _attach_tooltip(output_dir_label, _output_dir_hint)
        _attach_tooltip(output_dir_entry, _output_dir_hint)
        tk.Button(
            output_path_frame, text="Обзор", bg=BTN_COLOR, fg=TEXT_COLOR,
            relief="flat", font=("Arial", 9),
            command=lambda: self._browse_dir(self.output_dir_var)
        ).pack(side=tk.RIGHT)

        cb_debug = tk.Checkbutton(
            settings_inner, text="Режим отладки (сохранять промежуточные файлы)",
            variable=self.debug_mode,
            bg=BG_COLOR, fg=TEXT_COLOR, selectcolor=BTN_COLOR,
            font=("Arial", 11), bd=0, relief="flat",
            anchor="w", justify="left", wraplength=300
        )
        cb_debug.pack(anchor="w", pady=5)
        self._toggle_speaker_limits_visibility()
        self.root.after(50, self._style_combobox_popdowns)
        self.root.after(300, self._update_model_checkboxes)
        self.root.after(200, self._poll_background_jobs)
        self.root.after(180, self._animate_runtime_indicator)
        self.update_status()
        self.root.protocol("WM_DELETE_WINDOW", self.quit_app)

    def _style_combobox_popdowns(self):
        """Дополнительно стилизует выпадающие списки (Listbox) внутри Combobox в тёмной теме."""
        style = ttk.Style()
        style.configure("Dark.TCombobox", fieldbackground=BTN_COLOR)
        try:
            self.root.option_add('*TCombobox*Listbox.background', ENTRY_BG)
            self.root.option_add('*TCombobox*Listbox.foreground', TEXT_COLOR)
            self.root.option_add('*TCombobox*Listbox.selectBackground', BTN_COLOR)
            self.root.option_add('*TCombobox*Listbox.selectForeground', TEXT_COLOR)
        except Exception:
            pass

    def load_icon(self, path, size=(24, 24)):
        """Загружает изображение из ресурсов приложения и приводит его к размеру size."""
        full_path = os.path.join(RESOURCE_BASE_DIR, path.lstrip("./"))
        img = Image.open(full_path).resize(size, Image.Resampling.LANCZOS)
        return ImageTk.PhotoImage(img)

    def _effective_models_dir(self):
        """Каталог моделей: пользовательский или каталог программы."""
        value = self.models_dir_var.get().strip()
        return value or get_writable_base_dir()

    def _effective_output_dir(self):
        """Каталог результатов: пользовательский или каталог программы."""
        value = self.output_dir_var.get().strip()
        return value or get_writable_base_dir()

    def collect_settings(self) -> dict:
        """Единая точка сбора настроек: реестр «ключ -> переменная»."""
        collected = {}
        for key, var in self._setting_vars.items():
            value = var.get()
            if isinstance(value, str):
                value = value.strip()
            collected[key] = value
        return collected

    def _on_setting_change(self, *args):
        """Обрабатывает изменение любой настройки: сохраняет её на диске и применяет новый каталог моделей."""
        settings = self.collect_settings()
        save_settings(settings)

        models_dir = self._effective_models_dir()
        if models_dir:
            from model_manager import resolve_models_path as _resolve_models_path
            from background_tasks import apply_models_root
            models_path = _resolve_models_path(models_dir)
            os.makedirs(models_path, exist_ok=True)
            apply_models_root(models_path)
            if models_path != os.path.abspath(models_dir):
                self.log(f"Модели: найдены в {models_path}")

    def _browse_dir(self, var):
        """Открывает диалог выбора директории и записывает выбранный путь в переменную var."""
        path = filedialog.askdirectory(title="Выберите директорию")
        if path:
            var.set(path)

    def _on_diarization_toggle(self, *args):
        """Реакция на переключение диаризации — обновляет видимость полей ограничения спикеров."""
        self._toggle_speaker_limits_visibility()

    def _toggle_speaker_limits_visibility(self):
        """Показывает или скрывает блок с полями количества спикеров в зависимости от включённой диаризации."""
        if self.enable_diarization.get():
            self.speaker_limits_frame.pack(anchor="w", padx=8, pady=(0, 8))
        else:
            self.speaker_limits_frame.pack_forget()

    def _on_settings_mousewheel(self, event):
        """Прокручивает панель настроек колесом мыши, если открыта вкладка «Настройки»."""
        current_tab = self.notebook.select()
        if current_tab != str(self.tab_settings):
            return

        step = 0
        if getattr(event, "num", None) == 4:
            step = -1
        elif getattr(event, "num", None) == 5:
            step = 1
        elif getattr(event, "delta", 0):
            step = int(-(event.delta / 120))

        if step != 0:
            self.settings_canvas.yview_scroll(step, "units")
            return "break"

    def _set_buttons_recording(self, recording: bool, mode: str | None = None):
        """Обновляет состояние кнопок записи: сбрасывает их, а во время записи отключает неактивные и выделяет активную кнопку «Остановить»."""

        # Сначала сбрасываем все кнопки
        self.button_full.config(bg=BTN_COLOR, fg=TEXT_COLOR, text="Весь экран", image=self.fullscreen_img, state="normal", command=self.on_full_screen, cursor="hand2")
        self.button_area.config(bg=BTN_COLOR, fg=TEXT_COLOR, text="Область экрана", image=self.area_img, state="normal", command=self.on_area_screen, cursor="hand2")
        self.button_program.config(bg=BTN_COLOR, fg=TEXT_COLOR, text="Программа", image=self.program_img, state="normal", command=self.on_program_screen, cursor="hand2")
        self.button_audio.config(bg=BTN_COLOR, fg=TEXT_COLOR, text="Только аудио", image=self.empty_img, state="normal", command=self.on_audio_only, cursor="hand2")

        if recording and mode:
            self.recording_status_text.set("▶ ИДЁТ ЗАПИСЬ!")
            self.model_status_canvas.itemconfig(self.oval, fill="#EE5454")

            # Отключаем остальные кнопки записи
            if mode != "fullscreen":
                self.button_full.config(state="disabled", cursor="X_cursor")
            if mode != "area":
                self.button_area.config(state="disabled", cursor="X_cursor")
            if mode != "program":
                self.button_program.config(state="disabled", cursor="X_cursor")
            if mode != "audio":
                self.button_audio.config(state="disabled", cursor="X_cursor")

            # Активная кнопка остаётся включённой с текстом «Остановить»
            if mode == "fullscreen":
                self.button_full.config(text="Остановить", cursor="hand2")
            elif mode == "area":
                self.button_area.config(text="Остановить", cursor="hand2")
            elif mode == "program":
                self.button_program.config(text="Остановить", cursor="hand2")
            elif mode == "audio":
                self.button_audio.config(text="Остановить", cursor="hand2")
        else:
            self.update_status()

        self.root.update_idletasks()

    def log(self, message):
        """Добавляет сообщение в журнал и выводит его в текстовую область интерфейса."""
        self.global_log_lines.append(message)

        def _update_ui():
            self.log_text.config(state='normal')
            self.log_text.insert('end', message + '\n')
            self.log_text.see('end')
            self.log_text.config(state='disabled')

        try:
            self.root.after(0, _update_ui)
        except Exception:
            pass

    def set_model_status(self, state):
        """Обновляет индикатор состояния моделей, определяя, каких моделей не хватает для выбранных настроек."""
        text = "Модели не скачены"
        if isinstance(state, dict):
            required = []
            trans_enabled = bool(self.enable_transcription.get()) if hasattr(self, "enable_transcription") else True
            diar_enabled = bool(self.enable_diarization.get()) if hasattr(self, "enable_diarization") else True

            if trans_enabled:
                engine = self.transcription_engine.get() if hasattr(self, "transcription_engine") else "whisperx"
                required.append("vosk" if engine == "vosk" else "whisperx")

            if diar_enabled:
                diar_method = self.diarization_method.get() if hasattr(self, "diarization_method") else "diarize"
                if diar_method == "diarize":
                    required.append("pyannote")
                elif diar_method == "nemo":
                    required.append("nemo")

            if not trans_enabled and not diar_enabled:
                state = "ready"
                text = "Обработка отключена"
            else:
                missing = [name for name in required if not bool(state.get(name, False))]
                if missing:
                    state = "not_ready"
                    text = "Не хватает моделей: " + ", ".join(missing)
                else:
                    state = "ready"
                    text = "Модели готовы к работе"

        if state == "downloading":
            text = "Идёт скачивание"

        color = MODEL_STATUS_COLORS[state]
        self.model_status_color.set(color)
        self.model_status_text.set(text)
        if not self.current_mode["mode"]:
            self.recording_status_text.set(text)
            self.model_status_canvas.itemconfig(self.oval, fill=color)

    def update_status(self):
        """Обновляет строку статуса записи: показывает текущий режим, временный статус или состояние моделей."""
        if self.current_mode["mode"]:
            name = MODE_NAMES.get(self.current_mode["mode"], "")
            self.recording_status_text.set(f"Идёт запись: {name}")
            self.model_status_canvas.itemconfig(self.oval, fill=MODEL_STATUS_COLORS["ready"])
        elif self.runtime_status_override or self.model_download_jobs:
            self._render_runtime_status()
        else:
            state = get_model_status()
            self.set_model_status(state)

    def _set_runtime_status(self, message: str):
        """Устанавливает временный статус в строке состояния (например, о фоновой обработке)."""
        self.runtime_status_override = message
        self._render_runtime_status()

    def _clear_runtime_status(self):
        """Сбрасывает временный статус и заново обновляет строку состояния."""
        self.runtime_status_override = None
        self.update_status()

    def _clear_runtime_status_if_idle(self):
        """Снимает временный статус, только когда нет активной постобработки и записи."""
        if not self.postprocess_jobs and not self.current_mode["mode"]:
            self._clear_runtime_status()

    def _render_runtime_status(self):
        """Отрисовывает временный статус со спиннером в строке состояния, если запись не идёт."""
        if self.current_mode["mode"]:
            return

        if self.runtime_status_override:
            color = MODEL_STATUS_COLORS["ready"]
            message = self.runtime_status_override
        elif self.model_download_jobs:
            color = MODEL_STATUS_COLORS["downloading"]
            message = "Идёт скачивание моделей..."
        else:
            return

        frame = self._spinner_frames[self._spinner_index % len(self._spinner_frames)]
        self.recording_status_text.set(f"{frame} {message}")
        self.model_status_canvas.itemconfig(self.oval, fill=color)

    def _animate_runtime_indicator(self):
        """Циклически меняет анимацию индикатора и перерисовывает временный статус."""
        self._spinner_index = (self._spinner_index + 1) % len(self._spinner_frames)
        if (self.runtime_status_override or self.model_download_jobs) and not self.current_mode["mode"]:
            self._render_runtime_status()
        self.root.after(180, self._animate_runtime_indicator)

    def _start_model_download_process(self, model_name, token, models_dir=""):
        """Запускает фоновый процесс скачивания модели и регистрирует задачу для отслеживания прогресса."""
        if model_name in self.model_download_jobs:
            self.log(f"Скачивание {model_name} уже выполняется")
            return

        event_queue = self.mp_context.Queue()
        process = self.mp_context.Process(
            target=download_model_worker,
            args=(model_name, token, event_queue, models_dir),
            daemon=True,
        )
        self.model_download_jobs[model_name] = {
            "process": process,
            "queue": event_queue,
            "done": False,
        }
        process.start()

    def _start_postprocess_process(self, payload):
        """Запускает фоновый процесс постобработки записи и показывает статус «Идёт обработка»."""
        job_id = datetime.now().strftime("%Y%m%d%H%M%S%f")
        event_queue = self.mp_context.Queue()
        process = self.mp_context.Process(
            target=postprocess_recording_worker,
            args=(payload, event_queue),
            daemon=True,
        )
        self.postprocess_jobs[job_id] = {
            "process": process,
            "queue": event_queue,
            "done": False,
        }
        self._set_runtime_status("Идёт обработка записи...")
        process.start()

    def _poll_background_jobs(self):
        """Периодически опрашивает фоновые процессы (скачивание моделей и постобработка) и обновляет интерфейс по событиям из очереди."""
        finished_models = []
        for model_name, job in list(self.model_download_jobs.items()):
            q = job["queue"]
            while True:
                try:
                    event, payload = q.get_nowait()
                except queue.Empty:
                    break

                if event == "log":
                    self.log(payload)
                elif event == "progress":
                    percent = max(0.0, min(100.0, float(payload)))
                    bar = self.model_bars.get(model_name)
                    if bar is not None:
                        bar.stop()
                        bar.config(mode="determinate")
                        bar["value"] = percent
                elif event == "done":
                    ok = bool(payload.get("ok")) if isinstance(payload, dict) else False
                    job["done"] = True
                    if not ok:
                        bar = self.model_bars.get(model_name)
                        if bar is not None:
                            bar.stop()
                            bar.config(mode="determinate", value=0)
                elif event == "error":
                    self.log(f"Ошибка загрузки {model_name}: {payload}")
                elif event == "traceback":
                    self.log(payload)

            if job["done"] and not job["process"].is_alive():
                try:
                    job["process"].join(timeout=0.1)
                except Exception:
                    pass
                finished_models.append(model_name)
            elif not job["process"].is_alive() and not job["done"]:
                self.log(f"Скачивание {model_name} завершилось с ошибкой")
                finished_models.append(model_name)

        for model_name in finished_models:
            self.model_download_jobs.pop(model_name, None)
            self._update_model_checkboxes()
            self.update_status()

        finished_post = []
        for job_id, job in list(self.postprocess_jobs.items()):
            q = job["queue"]
            while True:
                try:
                    event, payload = q.get_nowait()
                except queue.Empty:
                    break

                if event == "log":
                    self.log(payload)
                elif event == "status":
                    self._set_runtime_status(payload)
                elif event == "done":
                    job["done"] = True
                    self.log("Фоновая обработка записи завершена")
                elif event == "error":
                    self.log(f"Ошибка фоновой обработки: {payload}")
                    self._set_runtime_status("Ошибка фоновой обработки")
                elif event == "traceback":
                    self.log(payload)

            if job["done"] and not job["process"].is_alive():
                try:
                    job["process"].join(timeout=0.1)
                except Exception:
                    pass
                finished_post.append(job_id)
            elif not job["process"].is_alive() and not job["done"]:
                self.log("Фоновая обработка завершилась с ошибкой")
                finished_post.append(job_id)

        for job_id in finished_post:
            self.postprocess_jobs.pop(job_id, None)

        if not self.postprocess_jobs and self.runtime_status_override:
            self._set_runtime_status("Обработка завершена")
            self.root.after(3000, self._clear_runtime_status_if_idle)

        self.root.after(200, self._poll_background_jobs)

    def stop_current(self):
        """Остановка записи и обработка результатов."""
        if self.current_mode["mode"] is None and self.current_instance["obj"] is None:
            return

        is_debug = self.debug_mode.get()
        stopped_obj = self.current_instance["obj"]

        if stopped_obj and hasattr(stopped_obj, "stop_recording"):
            try:
                stopped_obj.stop_recording()
            except Exception as e:
                self.log(f"Ошибка при остановке: {e}")

        self.current_instance["obj"] = None
        self.current_mode["mode"] = None
        self.audio_recorder.stop_recording()
        self._set_buttons_recording(False)

        mkv_files = [f for f in os.listdir('.') if f.endswith('.mkv')]
        wav_files = [f for f in os.listdir('.') if f.endswith('.wav')]
        main_video = None
        main_audio = None

        if mkv_files:
            main_video = max(mkv_files, key=os.path.getmtime)
        if "audio.wav" in wav_files:
            main_audio = "audio.wav"

        if main_audio:
            timestamp = safe_timestamp()
            selected_format = self.output_format.get().lower().strip()
            if selected_format not in OUTPUT_FORMATS:
                selected_format = "mp4"
            selected_video_quality = VIDEO_QUALITY_LABEL_TO_CRF.get(self.video_quality.get(), 23)
            selected_audio_mode = AUDIO_TRACK_LABEL_TO_MODE.get(self.audio_track_mode.get(), "copy")

            output_dir = self._effective_output_dir()
            if output_dir:
                os.makedirs(output_dir, exist_ok=True)

            final_video = os.path.join(output_dir, f"{timestamp}.{selected_format}") if main_video else None
            final_audio = os.path.join(output_dir, f"{timestamp}.wav") if (not main_video and output_dir) else None
            vid_audio_offset = self.audio_recorder.audio_silence_duration
            transcription_enabled = bool(self.enable_transcription.get())
            diarization_enabled = bool(self.enable_diarization.get()) and str(self.diarization_method.get()) != "none"
            txt_file = (
                os.path.join(output_dir, f"{timestamp}.txt")
                if output_dir and (transcription_enabled or diarization_enabled)
                else None
            )

            os.makedirs(".temp_postprocess", exist_ok=True)
            job_tag = str(time_module.time_ns())
            audio_src = os.path.abspath(main_audio)
            audio_tmp = os.path.abspath(os.path.join(".temp_postprocess", f"{job_tag}_audio.wav"))
            if is_debug:
                shutil.copy2(audio_src, audio_tmp)
            else:
                os.replace(audio_src, audio_tmp)

            video_tmp = None
            if main_video:
                video_src = os.path.abspath(main_video)
                video_tmp = os.path.abspath(os.path.join(".temp_postprocess", f"{job_tag}_video.mkv"))
                if is_debug:
                    shutil.copy2(video_src, video_tmp)
                else:
                    os.replace(video_src, video_tmp)

            models_dir = self._effective_models_dir()

            video_filter = None
            auto_crop = False
            wayland_compositor = ""
            crop = getattr(stopped_obj, '_crop', None) if stopped_obj else None
            is_wayland_mode = isinstance(stopped_obj, WaylandProgramScreenMode) if stopped_obj and WaylandProgramScreenMode is not None else False
            is_wayland_program = bool(is_wayland_mode and getattr(stopped_obj, "_source_types", None) == WINDOW_SOURCE_TYPE)
            if not is_wayland_mode:
                if crop and len(crop) == 4:
                    x0, y0, x1, y1 = crop
                    w = max(int(x1 - x0), 2)
                    h = max(int(y1 - y0), 2)
                    x = max(int(x0), 0)
                    y = max(int(y0), 0)
                    video_filter = f"crop={w}:{h}:{x}:{y}"
            elif is_wayland_program and not crop:
                auto_crop = True
                wayland_compositor = (
                    os.environ.get("XDG_CURRENT_DESKTOP")
                    or os.environ.get("XDG_SESSION_DESKTOP")
                    or os.environ.get("DESKTOP_SESSION")
                    or ""
                )

            payload = {
                "audio_file": audio_tmp,
                "video_file": video_tmp,
                "final_video": os.path.abspath(final_video) if final_video else None,
                "final_audio": os.path.abspath(final_audio) if final_audio else None,
                "audio_offset": vid_audio_offset,
                **self.collect_settings(),
                "use_vosk": self.transcription_engine.get() == "vosk",
                "txt_file": os.path.abspath(txt_file) if txt_file else None,
                "debug_mode": is_debug,
                "output_format": selected_format,
                "video_crf": selected_video_quality,
                "audio_mode": selected_audio_mode,
                "models_dir": models_dir,
                "video_filter": video_filter,
                "auto_crop": auto_crop,
                "wayland_compositor": wayland_compositor,
            }
            self.log(
                f"Экспорт видео: {selected_format.upper()}, CRF {selected_video_quality}, звук: {self.audio_track_mode.get()}"
            )
            self.log("Запущена фоновая обработка записи")
            self._start_postprocess_process(payload)
        else:
            self.log("Аудиофайл не найден: постобработка не запущена")

    def quit_app(self):
        """Завершает приложение: блокирует закрытие во время постобработки, иначе останавливает запись и закрывает окно."""
        if self.postprocess_jobs:
            self._set_runtime_status("Идёт распознавание, дождитесь завершения")
            self.log("Нельзя закрыть окно: выполняется фоновая обработка записи")
            return
        self.stop_current()
        self.root.destroy()


    def run_transcribe_and_diarization(self):
        """Обработка аудиофайла после записи (диаризация и распознавание)."""
        if not os.path.exists(AUDIO_OUTPUT):
            # Пробуем найти недавний аудиофайл
            import glob
            wav_files = glob.glob("*.wav")
            if not wav_files:
                self.log("Аудио файл не найден")
                return
            audio_file = max(wav_files, key=os.path.getmtime)
        else:
            audio_file = AUDIO_OUTPUT

        self.log(f"Обработка аудио: {audio_file}")

        if not os.path.exists(audio_file):
            self.log(f"Аудио файл не найден: {audio_file}")
            return

        timestamp = safe_timestamp()

        def _parse_optional_int(value):
            if value is None:
                return None
            if isinstance(value, str):
                value = value.strip()
                if not value:
                    return None
            try:
                ivalue = int(value)
            except Exception:
                return None
            return ivalue if ivalue > 0 else None

        output_dir = self._effective_output_dir()
        if output_dir:
            os.makedirs(output_dir, exist_ok=True)

        try:
            hf_token = load_hf_token()
            if not hf_token:
                self.log("Не найден HF Token")
                return

            use_vosk = self.transcription_engine.get() == "vosk"
            diar_method = self.diarization_method.get()
            expected_speakers = _parse_optional_int(self.expected_speakers.get())
            min_speakers = _parse_optional_int(self.min_speakers.get())
            max_speakers = _parse_optional_int(self.max_speakers.get())

            if not self.enable_transcription.get() and not self.enable_diarization.get():
                self.log("Транскрипция отключена в настройках")
                return

            diar_for_transcription = diar_method if self.enable_diarization.get() else "none"
            txt_file = os.path.join(output_dir, f"{timestamp}.txt") if output_dir else None

            # Диаризация недоступна в Vosk — отключаем, а не переключаем движок.
            if diar_for_transcription != "none" and use_vosk:
                self.log("Диаризация недоступна в Vosk — распознавание без разбиения по спикерам")
                diar_for_transcription = "none"

            if txt_file:
                use_vosk_final = use_vosk and diar_for_transcription == "none"
                note = (
                    " (спикеры требуют WhisperX — Vosk не используется)"
                    if use_vosk and diar_for_transcription != "none" else ""
                )
                self.log("Запуск распознавания речи..." + note)
                transcriber = AudioTranscriberService(
                    auth_token=hf_token,
                    whisper_model_path="faster-whisper-large-v3-turbo",
                    use_vosk=use_vosk_final,
                    use_whisperx=not use_vosk_final,
                    diarization_method=diar_for_transcription,
                    compute_device=self.compute_device.get() if hasattr(self, "compute_device") else "auto",
                    expected_speakers=expected_speakers,
                    min_speakers=min_speakers,
                    max_speakers=max_speakers,
                    include_timecodes=bool(self.include_timecodes.get()),
                )
                transcriber.full_transcribe(audio_file, txt_file)
                self.log(f"Текст сохранён: {txt_file}")

        except Exception as e:
            self.log(f"Ошибка транскрипции: {e}")

    def _run_transcribe(self, input_file: str):
        """Транскрибация аудиофайла."""
        self.log("Обработка аудио: диаризация и распознавание...")

        if not os.path.exists(input_file):
            self.log(f"Аудио файл не найден: {input_file}")
            return

        hf_token = load_hf_token()
        if not hf_token:
            self.log("Не найден HF Token в .hf_token")
            return

        whisper_model_path = "faster-whisper-large-v3-turbo"
        output_file = "output.md"
        use_vosk = self.transcription_engine.get() == "vosk"
        diar_method = self.diarization_method.get()

        try:
            transcriber = AudioTranscriberService(
                auth_token=hf_token,
                whisper_model_path=whisper_model_path,
                use_vosk=use_vosk,
                use_whisperx=not use_vosk,
                diarization_method=diar_method,
                include_timecodes=bool(self.include_timecodes.get()),
            )
            result = transcriber.full_transcribe(
                input_audio_file=AUDIO_OUTPUT,
                output_file=output_file,
                segment_length=15,
                append=False
            )
            self.log(f"Готово: {result}")
        except Exception as e:
            self.log(f"Ошибка: {e}")

    def _transcribe_realtime(self):
        """Транскрибация после записи (старый метод)."""
        import shutil
        import time

        self.log("Транскрибация запущена в фоне...")

        audio_file = "audio_realtime.wav"
        output_file = "output.md"

        # Ждём немного, чтобы накопилось аудио
        time.sleep(5)

        hf_token = load_hf_token()
        if not hf_token:
            self.log("Не найден HF Token")
            return

        try:
            use_vosk = self.transcription_engine.get() == "vosk"
            transcriber = AudioTranscriberService(
                auth_token=hf_token,
                whisper_model_path="faster-whisper-large-v3-turbo",
                use_vosk=use_vosk,
                use_whisperx=not use_vosk,
                diarization_method=self.diarization_method.get(),
                include_timecodes=bool(self.include_timecodes.get()),
            )

            # Ждём окончания записи и транскрибируем накопленное аудио
            while self.current_mode["mode"] is not None:
                if os.path.exists(AUDIO_OUTPUT) and os.path.getsize(AUDIO_OUTPUT) > 1000:
                    try:
                        shutil.copy(AUDIO_OUTPUT, audio_file)
                        self.log(f"Транскрибирую кусок ({os.path.getsize(audio_file)} байт)...")
                        result = transcriber.full_transcribe(
                            input_audio_file=audio_file,
                            output_file=output_file,
                            segment_length=180
                        )
                        self.log(f"Часть транскрибирована: {result}")
                    except Exception as e:
                        self.log(f"Частичная ошибка: {e}")
                time.sleep(10)

            # Финальная транскрибация после остановки записи
            if os.path.exists(AUDIO_OUTPUT) and os.path.getsize(AUDIO_OUTPUT) > 1000:
                self.log("Финальная транскрибация...")
                result = transcriber.full_transcribe(
                    input_audio_file=AUDIO_OUTPUT,
                    output_file=output_file,
                    segment_length=180
                )
                self.log(f"Распознавание завершено! Файл: {result}")

        except Exception as e:
            self.log(f"Ошибка транскрибации: {e}")

    def _start_mode(self, mode_class, mode_key, **kwargs):
        """Запускает выбранный режим записи: создаёт объект режима, начинает захват звука и запись, обновляет кнопки и статус."""
        self.stop_current()
        mode = mode_class(**kwargs)
        if hasattr(mode, "selected_area") and not mode.selected_area:
            self.current_instance["obj"] = None
            self.current_mode["mode"] = None
            self._set_buttons_recording(False)
            self.update_status()
            return
        self._start_audio_capture_or_warn()
        mode.start_recording()
        self.video_start_time = time_module.time()
        self._set_buttons_recording(True, mode_key)
        self.current_instance["obj"] = mode
        self.current_mode["mode"] = mode_key
        self.update_status()

    def _start_audio_capture_or_warn(self):
        """Запускает запись звука; при отсутствии ffmpeg или ошибках выводит предупреждения в журнал."""
        if not _check_disk_space():
            self.log("Предупреждение: мало места на диске. Запись может прерваться.")
        if not has_ffmpeg():
            self.log("ffmpeg не найден. Пробую скачать автоматически...")
            try:
                if ensure_ffmpeg():
                    self.log("ffmpeg успешно загружен в bin/<platform>.")
                else:
                    self.log("Не удалось получить ffmpeg. Добавьте бинарник в bin/<platform> или установите ffmpeg в систему.")
                    return
            except Exception as exc:
                self.log(f"Ошибка автоустановки ffmpeg: {exc}")
                return

        self.audio_recorder.start_all_sources()
        if self.audio_recorder.process_main is not None:
            return

        if sys.platform == "win32":
            self.log("Не удалось запустить запись звука (Windows). Проверьте аудио-устройства и ffmpeg.")
        elif sys.platform == "darwin":
            self.log("Не удалось запустить запись звука (macOS). Разрешите доступ к микрофону в Системных настройках → Приватность и безопасность → Микрофон. Для записи системного звука установите виртуальное аудио-устройство (например BlackHole).")
        else:
            self.log("Не удалось запустить запись звука. Проверьте аудио-устройства и ffmpeg.")

    def on_full_screen(self):
        """Обработчик кнопки «Весь экран»: останавливает запись или запускает режим записи всего экрана (в т.ч. через Wayland)."""
        if self.current_mode["mode"] == "fullscreen":
            self.stop_current()
            return
        self.stop_current()

        use_wayland = False
        if WaylandFullScreenMode is not None and is_wayland_session():
            use_wayland = WaylandFullScreenMode.is_supported()
            if not use_wayland:
                self._handle_wayland_missing("fullscreen")
                self.update_status()
                return

        if use_wayland:
            self.recording_status_text.set("Запрос доступа к экрану через портал Wayland...")

            def on_start(is_recording: bool):
                if self.current_instance["obj"] is not mode:
                    return
                if is_recording:
                    self.current_mode["mode"] = "fullscreen"
                    self._start_audio_capture_or_warn()
                    self._set_buttons_recording(True, "fullscreen")
                else:
                    self.current_mode["mode"] = None
                    self.current_instance["obj"] = None
                    self.audio_recorder.stop_recording()
                    self._set_buttons_recording(False)
                self.update_status()

            crf = VIDEO_QUALITY_LABEL_TO_CRF.get(self.video_quality.get(), 23)
            mode = WaylandFullScreenMode(self.root, on_start, logger=self.log, video_crf=crf)
            self.current_instance["obj"] = mode
            self._set_buttons_recording(True, "fullscreen")
        else:
            self.recording_status_text.set("Идёт запись всего экрана...")
            self._set_buttons_recording(True, "fullscreen")
            self._start_mode(FullScreenMode, "fullscreen")

    def on_area_screen(self):
        """Обработчик кнопки «Область экрана»: останавливает запись или запускает запись выделенной области (в т.ч. через Wayland)."""
        if self.current_mode["mode"] == "area":
            self.stop_current()
            return

        self.stop_current()
        use_wayland = False
        if WaylandAreaScreenMode is not None and is_wayland_session():
            use_wayland = WaylandAreaScreenMode.is_supported()
            if not use_wayland:
                self._handle_wayland_missing("area")
                self.update_status()
                return

        if use_wayland:
            area = select_wayland_area()
            if not area:
                area = select_screen_area(master=self.root)
            if area:
                self.recording_status_text.set("Запрос доступа к экрану через портал Wayland...")

                def on_start(is_recording: bool):
                    if self.current_instance["obj"] is not mode:
                        return
                    if is_recording:
                        self.current_mode["mode"] = "area"
                        self._start_audio_capture_or_warn()
                        self._set_buttons_recording(True, "area")
                    else:
                        self.current_mode["mode"] = None
                        self.current_instance["obj"] = None
                        self.audio_recorder.stop_recording()
                        self._set_buttons_recording(False)
                    self.update_status()

                crf = VIDEO_QUALITY_LABEL_TO_CRF.get(self.video_quality.get(), 23)
                mode = WaylandAreaScreenMode(self.root, on_start, area=area, logger=self.log, video_crf=crf)
                self.current_instance["obj"] = mode
                self._set_buttons_recording(True, "area")
                return

            self.log("Wayland (area): выбор области недоступен")
            return

        self._set_buttons_recording(True, "area")
        self._start_mode(AreaScreenMode, "area", master=self.root)

    def on_program_screen(self):
        """Обработчик кнопки «Программа»: останавливает запись или запускает запись выбранного окна/программы."""
        if self.current_mode["mode"] == "program":
            self.stop_current()
            return
        self.stop_current()
        use_wayland = False
        if WaylandProgramScreenMode is not None and is_wayland_session():
            use_wayland = WaylandProgramScreenMode.is_supported()
            if not use_wayland:
                self._handle_wayland_missing("program")
                self.update_status()
                return

        def on_start(is_recording):
            if self.current_instance["obj"] is not mode:
                return
            if is_recording:
                self.current_mode["mode"] = "program"
                self._start_audio_capture_or_warn()
                self._set_buttons_recording(True, "program")
            else:
                self.current_mode["mode"] = None
                self.current_instance["obj"] = None
                self.audio_recorder.stop_recording()
                self._set_buttons_recording(False)
            self.update_status()
        if use_wayland:
            self.recording_status_text.set("Запрос доступа к окну через портал Wayland...")
            crf = VIDEO_QUALITY_LABEL_TO_CRF.get(self.video_quality.get(), 23)
            mode = WaylandProgramScreenMode(self.root, on_start, logger=self.log, video_crf=crf)
        elif sys.platform == "win32":
            self.recording_status_text.set("Выберите окно для записи...")
            mode = WindowsProgramScreenMode(self.root, on_start)
        else:
            self.recording_status_text.set("Выберите окно для записи...")
            mode = ProgramScreenMode(self.root, on_start)
        self.current_instance["obj"] = mode
        self._set_buttons_recording(True, "program")

    def _handle_wayland_missing(self, context: str):
        """Фиксирует отсутствие Wayland-поддержки: пишет отчёт об ошибке и сообщает пользователю в статус и журнал."""
        details = wayland_dependency_issue() or "неизвестно"
        if details == "модуль Wayland недоступен" and _WAYLAND_BACKEND_IMPORT_ERROR:
            details = f"не удалось импортировать backend Wayland: {_WAYLAND_BACKEND_IMPORT_ERROR}"
        msg = f"Wayland ({context}) недоступен: {details}"
        log_path = write_error_report("wayland", msg)
        info = f"Wayland: запись недоступна ({context}), подробности в {log_path}"
        self.recording_status_text.set(info)
        self.log(info)

    def on_audio_only(self):
        """Обработчик кнопки «Только аудио»: запускает или останавливает запись только звука."""
        if self.current_mode["mode"] == "audio":
            self.stop_current()
        else:
            self.stop_current()
            self.current_mode["mode"] = "audio"
            self._start_audio_capture_or_warn()
            self._set_buttons_recording(True, "audio")
            self.update_status()

    def _set_model_row_state(self, model_name, *, downloaded=None, downloading=False):
        """Состояние строки модели: кнопки + прогрессбар.

        downloading: обе кнопки неактивны, бар «полоса бежит».
        downloaded:  «Скачать» -> отключена с текстом «Скачано ✓», «Удалить» активна.
        иначе:       «Скачать» активна, «Удалить» отключена, бар пустой.
        """
        buttons = self.model_buttons.get(model_name)
        bar = self.model_bars.get(model_name)
        if not buttons or bar is None:
            return

        if downloading:
            buttons["download"].config(state="disabled", text="Скачивание...")
            buttons["delete"].config(state="disabled")
            bar.stop()
            bar.config(mode="indeterminate", value=0)
            bar.start(12)
            return

        if downloaded:
            buttons["download"].config(state="disabled", text="Скачано ✓")
            buttons["delete"].config(state="normal")
            bar.stop()
            bar.config(mode="determinate", value=100)
        else:
            buttons["download"].config(state="normal", text="Скачать")
            buttons["delete"].config(state="disabled")
            bar.stop()
            bar.config(mode="determinate", value=0)

    def _refresh_model_rows(self):
        """Обновить все строки моделей по фактическому состоянию на диске."""
        from model_manager import get_model_status

        status = get_model_status()
        for name in ("whisperx", "vosk", "pyannote", "nemo"):
            self._set_model_row_state(name, downloaded=bool(status.get(name)))
        self.set_model_status(status)

    def _update_model_checkboxes(self):
        """Обновляет состояние строк моделей по фактическому состоянию файлов на диске."""
        self._refresh_model_rows()

    def _delete_model(self, model_name):
        """Удалить файлы модели с диска (с подтверждением)."""
        if model_name in self.model_download_jobs:
            return
        from tkinter import messagebox

        from model_manager import delete_model as mm_delete_model

        title = self._model_titles.get(model_name, model_name)
        if not messagebox.askyesno(
            "Удаление модели",
            f"Удалить файлы модели «{title}» с диска?",
        ):
            return
        try:
            removed = mm_delete_model(model_name)
            if removed:
                self.log(f"Модель {title} удалена")
            else:
                self.log(f"Файлы модели {title} не найдены")
        except Exception as e:
            self.log(f"Ошибка удаления {title}: {e}")
        self._refresh_model_rows()

    def _download_model(self, model_name):
        """Запуск скачивания модели по кнопке «Скачать»."""
        from model_manager import load_hf_token, save_hf_token

        if model_name in self.model_download_jobs:
            return

        needs_token = model_name in {"pyannote"}
        hf_token = None
        if needs_token:
            hf_token = load_hf_token()
            if not hf_token:
                token = ask_hf_token_gui(self.root)
                if not token:
                    self.log("❌ Нужен токен HuggingFace")
                    return
                save_hf_token(token)
                hf_token = token

        self._set_model_row_state(model_name, downloading=True)
        self._start_model_download_process(model_name, hf_token, self.models_dir_var.get().strip())

    def run(self):
        """Запускает проверку ffmpeg при старте и входит в главный цикл обработки событий Tkinter."""
        self.root.after(500, self._startup_ffmpeg_check)
        self.root.mainloop()

    def _startup_ffmpeg_check(self):
        """При старте проверяет наличие ffmpeg и при отсутствии пытается автоматически его скачать в фоне."""
        if has_ffmpeg():
            return
        self.log("ffmpeg не найден. Пробую скачать автоматически...")
        def _do():
            try:
                if ensure_ffmpeg():
                    self.root.after(0, lambda: self.log("ffmpeg успешно загружен."))
                else:
                    self.root.after(0, lambda: self.log(
                        "Не удалось скачать ffmpeg. Запись видео будет недоступна."))
            except Exception as exc:
                self.root.after(0, lambda e=exc: self.log(f"Ошибка автоустановки ffmpeg: {e}"))
        import threading
        threading.Thread(target=_do, daemon=True).start()
