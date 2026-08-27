import glob
import os
import sys
import traceback
from datetime import datetime


class DebugLogRedirect:
    """Лог-файл режима отладки.

    Перенаправляет stdout/stderr дочернего процесса (NeMo, ffmpeg и т.д.)
    в файл, чтобы консоль оставалась чистой. Наши log()/status() дублируются
    сюда же с метками времени.
    """

    def __init__(self, path):
        self.path = path
        self._file = None
        self._saved = (sys.stdout, sys.stderr)

    def open(self):
        os.makedirs(os.path.dirname(os.path.abspath(self.path)), exist_ok=True)
        self._file = open(self.path, "a", encoding="utf-8", buffering=1)
        sys.stdout = self._file
        sys.stderr = self._file

    def write_line(self, text):
        if self._file is None or self._file.closed:
            return
        ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        for line in str(text).splitlines() or [""]:
            self._file.write(f"[{ts}] {line}\n")

    def close(self):
        sys.stdout, sys.stderr = self._saved
        if self._file is not None and not self._file.closed:
            try:
                self._file.flush()
                self._file.close()
            except Exception:
                pass


def _debug_log_path(payload) -> str:
    """Файл лога кладём рядом с результатами записи (<штамп>.log)."""
    for candidate in (payload.get("txt_file"), payload.get("final_video"), payload.get("final_audio")):
        if candidate:
            directory = os.path.dirname(os.path.abspath(candidate))
            stem = os.path.splitext(os.path.basename(candidate))[0]
            return os.path.join(directory, f"{stem}.log")
    return os.path.join("logs", "postprocess_debug.log")


def apply_models_root(models_path):
    """Применить корень моделей ко всем менеджерам (в текущем процессе)."""
    from model_manager import set_model_dir as _mm_set_model_dir
    from audio_transcriber_service import set_model_dir as _ats_set_model_dir
    _mm_set_model_dir(models_path)
    _ats_set_model_dir(models_path)


def _apply_models_dir(models_dir):
    """Настройка «Папка моделей» указывает родительский каталог.

    Резолвим фактический корень (с учётом уже скачанных моделей в других
    раскладках) и применяем к обоим менеджерам. Вызывать в дочернем
    (spawn) процессе: состояние set_model_dir из родителя не переносится.
    """
    models_dir = str(models_dir or "").strip()
    if not models_dir:
        return
    from model_manager import resolve_models_path
    apply_models_root(resolve_models_path(models_dir))


def download_model_worker(model_name, token, event_queue, models_dir=""):
    from proc_env import install_subprocess_guard
    install_subprocess_guard()

    from model_manager import download_model

    try:
        _apply_models_dir(models_dir)
    except Exception:
        pass

    def log_func(message):
        event_queue.put(("log", str(message)))

    def progress_func(percent):
        event_queue.put(("progress", float(percent)))

    try:
        ok = bool(download_model(model_name, token, log_func, progress_func))
        event_queue.put(("done", {"ok": ok}))
    except Exception as exc:
        event_queue.put(("error", str(exc)))
        event_queue.put(("traceback", traceback.format_exc()))


def postprocess_recording_worker(payload, event_queue):
    from proc_env import install_subprocess_guard
    install_subprocess_guard()

    from audio_transcriber_service import AudioTranscriberService
    from merge_save import detect_crop, merge_av
    from model_manager import is_model_downloaded, load_hf_token

    def log(message):
        event_queue.put(("log", str(message)))
        if debug_redirect:
            debug_redirect.write_line(message)

    def status(message):
        event_queue.put(("status", str(message)))
        if debug_redirect:
            debug_redirect.write_line(f"[СТАТУС] {message}")

    def parse_optional_int(value):
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

    audio_file = payload.get("audio_file")
    video_file = payload.get("video_file")
    final_video = payload.get("final_video")
    final_audio = payload.get("final_audio")
    audio_offset = payload.get("audio_offset")
    use_vosk = bool(payload.get("use_vosk", False))
    diarization_method = payload.get("diarization_method", "diarize")
    enable_transcription = bool(payload.get("enable_transcription", True))
    enable_diarization = bool(payload.get("enable_diarization", True))
    expected_speakers = parse_optional_int(payload.get("expected_speakers"))
    min_speakers = parse_optional_int(payload.get("min_speakers"))
    max_speakers = parse_optional_int(payload.get("max_speakers"))
    txt_file = payload.get("txt_file")
    include_timecodes = bool(payload.get("include_timecodes", False))
    debug_mode = bool(payload.get("debug_mode", False))
    debug_log_path = _debug_log_path(payload) if debug_mode else None
    debug_redirect: DebugLogRedirect | None = None
    output_format = str(payload.get("output_format", "mp4")).lower().strip()
    video_crf = payload.get("video_crf", 23)
    audio_mode = payload.get("audio_mode", "copy")
    video_filter = payload.get("video_filter") or None
    auto_crop = bool(payload.get("auto_crop", False))
    wayland_compositor = str(payload.get("wayland_compositor", "")).strip().lower()
    models_dir = str(payload.get("models_dir", "")).strip()

    if models_dir:
        try:
            _apply_models_dir(models_dir)
        except Exception:
            pass

    try:
        video_crf = int(video_crf)
    except Exception:
        video_crf = 23

    try:
        if debug_mode:
            debug_redirect = DebugLogRedirect(debug_log_path)
            debug_redirect.open()
            debug_redirect.write_line("=" * 70)
            debug_redirect.write_line(f"РЕЖИМ ОТЛАДКИ | {datetime.now():%Y-%m-%d %H:%M:%S}")
            speakers_info = expected_speakers or "%s-%s" % (
                min_speakers or "?", max_speakers or "?",
            )
            debug_redirect.write_line(
                "Параметры: "
                f"движок={'vosk' if use_vosk else 'whisperx'}, "
                f"транскрипция={enable_transcription}, диаризация={enable_diarization} ({diarization_method}), "
                f"спикеры={speakers_info}, "
                f"метки={include_timecodes}, формат={output_format}/CRF{video_crf}/звук:{audio_mode}"
            )
            debug_redirect.write_line(
                f"Файлы: видео={video_file}, аудио={audio_file}, текст={txt_file}, модели={models_dir or 'по умолчанию'}"
            )

        status("Идёт обработка записи...")

        if auto_crop and not video_filter and video_file:
            status("Определяем область окна...")
            sample_seconds = 3.0 if "gnome" in wayland_compositor else 2.0
            crop = detect_crop(video_file, sample_seconds=sample_seconds)
            if crop:
                video_filter = f"crop={crop}"
                log(f"Автообрезка окна: {video_filter}")
            else:
                log("Автообрезка окна не сработала: область не найдена")

        if video_file and audio_file and final_video:
            status("Объединяем видео и аудио...")
            merged = merge_av(
                video_file,
                audio_file,
                final_video,
                audio_offset=audio_offset,
                output_format=output_format,
                video_crf=video_crf,
                audio_mode=audio_mode,
                video_filter=video_filter,
            )
            if merged:
                log(f"Видео сохранено: {merged}")
            else:
                log("Не удалось объединить видео и аудио")
                if os.path.getsize(video_file) > 1024:
                    log("Пробуем восстановить видео...")
                    repaired = os.path.join(os.path.dirname(final_video), "_repaired_" + os.path.basename(final_video))
                    try:
                        import subprocess

                        from ffmpeg_locator import ffmpeg_command
                        from merge_save import _probe_duration_seconds

                        ffmpeg_bin = ffmpeg_command()
                        subprocess.run(
                            [
                                ffmpeg_bin, "-y",
                                "-fflags", "+genpts+igndts",
                                "-i", video_file,
                                "-c", "copy",
                                repaired,
                            ],
                            capture_output=True, timeout=60, check=False,
                        )
                        duration = _probe_duration_seconds(repaired) if os.path.exists(repaired) else None
                        if (
                            os.path.exists(repaired)
                            and os.path.getsize(repaired) > 1024
                            and duration
                            and duration >= 0.5
                        ):
                            os.replace(repaired, final_video)
                            log(f"Видео восстановлено: {final_video} ({duration:.1f}s)")
                        else:
                            log(
                                "Восстановление видео не удалось: файл повреждён или слишком короткий"
                            )
                            try:
                                os.remove(repaired)
                            except Exception:
                                pass
                    except Exception as exc:
                        log(f"Не удалось восстановить видео: {exc}")
        elif audio_file and final_audio and not video_file:
            status("Сохраняем аудио...")
            try:
                import shutil

                os.makedirs(os.path.dirname(final_audio) or ".", exist_ok=True)
                shutil.copy2(audio_file, final_audio)
                log(f"Аудио сохранено: {final_audio}")
            except Exception as exc:
                log(f"Не удалось сохранить аудио: {exc}")

        if not enable_transcription and not enable_diarization:
            log("Транскрипция отключена в настройках")
        elif not txt_file:
            log("Папка вывода не задана: текст не сохранён")
        else:
            hf_token = load_hf_token() or ""
            engine = str(payload.get("transcription_engine", "")).strip()
            if not engine:
                engine = "vosk" if use_vosk else "whisperx"

            speakers_requested = (
                enable_diarization and diarization_method not in ("", "none")
            )

            # Спикеры доступны только в WhisperX (пословные таймкоды).
            # При выборе Vosk — отключаем диаризацию, а не переключаем движок.
            if speakers_requested and use_vosk:
                log("Диаризация недоступна в Vosk — распознавание без разбиения по спикерам")
                speakers_requested = False
                diarization_method = "none"
            use_vosk_final = bool(use_vosk) and not speakers_requested

            if speakers_requested:
                if diarization_method == "sherpa":
                    diarization_method = "nemo"
                model_key = {"diarize": "pyannote", "nemo": "nemo"}.get(diarization_method)
                if model_key is None:
                    log(f"Неизвестный метод спикеров '{diarization_method}', разбиение отключено")
                    speakers_requested = False
                    diarization_method = "none"
                elif diarization_method == "diarize" and not hf_token:
                    log("HF_TOKEN не найден: разбиение по спикерам (PyAnnote) пропущено")
                    speakers_requested = False
                    diarization_method = "none"
                elif not is_model_downloaded(model_key):
                    log(f"Модель для метода '{diarization_method}' не скачена, разбиение по спикерам пропущено")
                    speakers_requested = False
                    diarization_method = "none"

            asr_model_key = "vosk" if use_vosk_final else "whisperx"
            if not is_model_downloaded(asr_model_key):
                log(f"Модель {'Vosk' if use_vosk_final else 'WhisperX'} не скачена, распознавание текста пропущено")
            else:
                try:
                    label = "Распознаём текст"
                    if speakers_requested:
                        label += f" и спикеров ({diarization_method})"
                    status(f"{label}...")
                    transcriber = AudioTranscriberService(
                        auth_token=hf_token or None,
                        whisper_model_path="faster-whisper-large-v3-turbo",
                        use_vosk=use_vosk_final,
                        use_whisperx=not use_vosk_final,
                        diarization_method=diarization_method if speakers_requested else "none",
                        log_callback=log,
                        expected_speakers=expected_speakers,
                        min_speakers=min_speakers,
                        max_speakers=max_speakers,
                        include_timecodes=include_timecodes,
                        compute_device=str(payload.get("compute_device", "auto")).strip().lower(),
                    )
                    transcriber.full_transcribe(audio_file, txt_file)
                    log(f"Текст сохранён: {txt_file}")
                except Exception as exc:
                    log(f"Ошибка распознавания текста: {exc}")

        status("Фоновая обработка завершена")
        event_queue.put(("done", {"ok": True}))
    except Exception as exc:
        event_queue.put(("error", str(exc)))
        event_queue.put(("traceback", traceback.format_exc()))
    finally:
        if debug_redirect is not None:
            debug_redirect.write_line(f"[{datetime.now():%Y-%m-%d %H:%M:%S}] Обработка завершена, лог закрыт")
            debug_redirect.close()
            debug_redirect = None
        if not debug_mode:
            cleanup_targets = [video_file, audio_file]
            for target in cleanup_targets:
                if target and os.path.exists(target):
                    try:
                        os.remove(target)
                    except Exception:
                        pass

            for file_path in glob.glob("*_vosk.wav"):
                try:
                    os.remove(file_path)
                except Exception:
                    pass

            for file_path in glob.glob(os.path.join(".temp_segments", "*.wav")):
                try:
                    os.remove(file_path)
                except Exception:
                    pass
