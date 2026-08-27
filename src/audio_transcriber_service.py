import inspect
import json
import math
import os
import shutil
import subprocess
import sys
import uuid
import warnings
import wave
from collections.abc import Callable
from datetime import timedelta

import numpy as np
import soundfile as sf
import torch
from scipy import signal

from ffmpeg_locator import ffmpeg_command

os.environ["TOKENIZERS_PARALLELISM"] = "false"
warnings.filterwarnings("ignore")

WhisperProcessor = None
WhisperForConditionalGeneration = None
try:
    from transformers import logging as tf_logging
    tf_logging.set_verbosity_error()
except Exception:
    tf_logging = None


from app_paths import get_writable_base_dir

BASE_DIR = get_writable_base_dir()
PYTHON_VERSION_TAG = f"python{sys.version_info.major}.{sys.version_info.minor}"
LOCAL_SITE_PACKAGES = os.path.join(BASE_DIR, ".venv", "lib", PYTHON_VERSION_TAG, "site-packages")
if os.path.isdir(LOCAL_SITE_PACKAGES) and LOCAL_SITE_PACKAGES not in sys.path:
    # Frozen builds prefer the bundled venv. In normal/dev runs append so a
    # working user-site transformers is not shadowed by a broken project .venv.
    if getattr(sys, "frozen", False):
        sys.path.insert(0, LOCAL_SITE_PACKAGES)
    else:
        sys.path.append(LOCAL_SITE_PACKAGES)
TORCH_LIB_DIR = os.path.join(LOCAL_SITE_PACKAGES, "torch", "lib")
if os.path.isdir(TORCH_LIB_DIR):
    current_ld = os.environ.get("LD_LIBRARY_PATH", "")
    paths = current_ld.split(":") if current_ld else []
    if TORCH_LIB_DIR not in paths:
        new_ld = f"{TORCH_LIB_DIR}:{current_ld}" if current_ld else TORCH_LIB_DIR
        os.environ["LD_LIBRARY_PATH"] = new_ld
_MODEL_DIR = os.path.join(BASE_DIR, "model")

def _build_vosk_paths(model_dir):
    return [
        os.path.join(model_dir, "vosk-model-ru-0.42.zip"),
        os.path.join(model_dir, "vosk-model-ru-0.42"),
        os.path.join(model_dir, "vosk_model"),
        os.path.join(model_dir, "vosk_model", "vosk-model-ru-0.42"),
        os.path.join(model_dir, "vosk_model", "vosk-model-small-ru-0.22"),
    ]

VOSK_MODEL_PATHS = _build_vosk_paths(_MODEL_DIR)

NEMO_MODEL_DIRNAME = "diar_sortformer_4spk-v1"
NEMO_MODEL_FILE = "diar_sortformer_4spk-v1.nemo"
NEMO_MAX_WINDOW_SEC = 480.0
NEMO_WINDOW_OVERLAP_SEC = 60.0

# Минимум свободной видеопамяти для запуска модели на GPU.
# Если свободного VRAM меньше — модель уходит на CPU.
WHISPERX_VRAM_REQUIRED_GB = 4.0
NEMO_VRAM_REQUIRED_GB = 3.0

def _build_nemo_model_paths(model_dir):
    return [
        os.path.join(model_dir, NEMO_MODEL_DIRNAME, NEMO_MODEL_FILE),
        os.path.join(model_dir, NEMO_MODEL_FILE),
        getattr(sys, "_MEIPASS", None) and os.path.join(sys._MEIPASS, NEMO_MODEL_FILE),
    ]

NEMO_MODEL_PATHS = _build_nemo_model_paths(_MODEL_DIR)

def set_model_dir(model_dir):
    global _MODEL_DIR, VOSK_MODEL_PATHS, NEMO_MODEL_PATHS
    _MODEL_DIR = model_dir
    VOSK_MODEL_PATHS = _build_vosk_paths(_MODEL_DIR)
    NEMO_MODEL_PATHS = _build_nemo_model_paths(_MODEL_DIR)


def _patch_torchaudio_load():
    """torchaudio >= 2.9 требует torchcodec для чтения аудиофайлов.

    Ряд зависимостей (silero-vad в пакете diarize, wespeakerruntime)
    читает аудио через torchaudio.load и молча падает без torchcodec.
    Подменяем torchaudio.load на реализацию через soundfile.
    Возвращает тензор [каналы, кадры] и частоту, как оригинал.
    Идемпотентно: повторный вызов ничего не меняет.
    """
    try:
        from packaging.version import parse as _vparse

        import torchaudio

        if _vparse(torchaudio.__version__) < _vparse("2.9"):
            return
        if getattr(torchaudio.load, "_soundfile_patch", False):
            return

        import soundfile as _sf
        import torch as _torch

        def _load_soundfile(uri, *args, **kwargs):
            data, sr = _sf.read(str(uri), dtype="float32", always_2d=True)
            return _torch.from_numpy(data.T), sr

        _load_soundfile._soundfile_patch = True
        torchaudio.load = _load_soundfile
    except Exception:
        pass


class AudioTranscriberService:
    """
    Сервис для транскрибирования аудиофайлов с диаризацией спикеров.
    Поддерживает Whisper и Vosk.
    """

    def __init__(
        self,
        auth_token: str,
        whisper_model_path="whisper-medium",
        use_vosk=False,
        use_whisperx=False,
        diarization_method="none",
        log_callback: Callable[[str], None] | None = None,
        expected_speakers: int | None = None,
        min_speakers: int | None = None,
        max_speakers: int | None = None,
        include_timecodes=False,
        compute_device="auto",
    ):
        _patch_torchaudio_load()
        self.auth_token = auth_token
        self.whisper_model_path = whisper_model_path
        self.use_vosk = use_vosk
        self.use_whisperx = use_whisperx
        if diarization_method == "sherpa":
            diarization_method = "nemo"
        self.diarization_method = diarization_method
        self.include_timecodes = bool(include_timecodes)
        self._log_callback = log_callback
        self.expected_speakers = self._normalize_speaker_count(expected_speakers)
        self.min_speakers = self._normalize_speaker_count(min_speakers)
        self.max_speakers = self._normalize_speaker_count(max_speakers)
        self._sanitize_speaker_constraints()
        self.compute_device = (compute_device or "auto").strip().lower()
        self.device = self._get_safe_device()
        self.speaker_mapping = {}
        self.speaker_counter = 0

        self.processor = None
        self.model = None
        self._vosk_model = None
        self._whisperx_model = None
        self._whisperx_import_error = None
        self._nemo_model = None
        self._nemo_import_error = None
        self._vad_ready = False
        self._vad_check_done = False
        self._whisper_import_error = None
        if diarization_method == "diarize":
            self._ensure_vad_ready()

    def _normalize_speaker_count(self, value) -> int | None:
        if value is None:
            return None
        if isinstance(value, str):
            value = value.strip()
            if not value:
                return None
        try:
            count = int(value)
        except Exception:
            return None
        if count <= 0:
            return None
        return count

    def _sanitize_speaker_constraints(self):
        if self.expected_speakers is not None:
            self.min_speakers = self.expected_speakers
            self.max_speakers = self.expected_speakers
            return
        if self.min_speakers is not None and self.max_speakers is not None and self.min_speakers > self.max_speakers:
            self.min_speakers, self.max_speakers = self.max_speakers, self.min_speakers

    def _check_nemo_dependencies(self) -> bool:
        for path in NEMO_MODEL_PATHS:
            if path and os.path.isfile(path):
                return True
        self._log_progress(
            f"NeMo недоступен: отсутствует файл модели {NEMO_MODEL_FILE}"
        )
        return False

    def _ensure_nemo_ready(self) -> bool:
        try:
            import nemo_compat
            nemo_compat.ensure_nemo_imports()
            import nemo.collections.asr  # noqa: F401
            return True
        except Exception as exc:
            self._nemo_import_error = str(exc)
            self._log_progress(f"NeMo недоступен: {exc}")
            return False

    def _load_nemo_diarizer(self):
        """Sortformer (NVIDIA NeMo): офлайн-диаризация до 4 спикеров."""
        if self._nemo_model is not None:
            return self._nemo_model
        if not self._check_nemo_dependencies():
            raise RuntimeError(f"Файл модели NeMo не найден: {NEMO_MODEL_FILE}")
        if not self._ensure_nemo_ready():
            raise RuntimeError(
                f"Не удалось загрузить NeMo{': ' + self._nemo_import_error if self._nemo_import_error else ''}"
            )

        from nemo.collections.asr.models import SortformerEncLabelModel

        last_error = None
        for path in NEMO_MODEL_PATHS:
            if not path or not os.path.isfile(path):
                continue
            try:
                self._log_progress(f"Загрузка NeMo Sortformer: {path}")
                model = SortformerEncLabelModel.restore_from(
                    restore_path=path, map_location="cpu", strict=False
                )
                # Сначала проверяем видеопамять, только потом решаем GPU/CPU.
                if self._gpu_has_vram(NEMO_VRAM_REQUIRED_GB, "NeMo"):
                    model.to("cuda")
                model.eval()
                self._nemo_model = model
                return self._nemo_model
            except Exception as e:
                last_error = e
                self._nemo_model = None
        raise RuntimeError(f"Не удалось загрузить модель NeMo: {last_error}")

    def _parse_sortformer_segments(self, raw_segments, offset_sec=0.0) -> list[dict]:
        """Строки 'begin end speaker_N' -> сегменты в секундах."""
        segments = []
        for line in raw_segments or []:
            try:
                if isinstance(line, str):
                    parts = line.replace(",", " ").split()
                    start = float(parts[0])
                    end = float(parts[1])
                    tail = parts[2] if len(parts) > 2 else "0"
                    digits = "".join(ch for ch in tail if ch.isdigit())
                    speaker_idx = int(digits) if digits else 0
                else:
                    start = float(line[0])
                    end = float(line[1])
                    speaker_idx = int(line[2])
            except Exception:
                continue
            start += offset_sec
            end += offset_sec
            if end <= start:
                continue
            segments.append({
                "speaker": f"Спикер {speaker_idx + 1}",
                "start": max(0.0, start),
                "end": end,
            })
        return segments

    @staticmethod
    def _match_speaker_label(label_a: str, label_b: str, segs_a: list[dict], segs_b: list[dict]) -> float:
        """Схожесть двух спикеров по перекрытию их сегментов (секунды)."""
        overlap = 0.0
        total = 0.0
        for a in segs_a:
            total += a["end"] - a["start"]
            for b in segs_b:
                lo = max(a["start"], b["start"])
                hi = min(a["end"], b["end"])
                if hi > lo:
                    overlap += hi - lo
        if total <= 0.0:
            return 0.0
        return overlap / total

    def _merge_windowed_segments(self, window_results: list[list[dict]]) -> list[dict]:
        """Склеивает сегменты соседних окон, сопоставляя локальные метки
        спикеров (порядок у Sortformer может отличаться между окнами)."""
        merged: list[dict] = []
        global_labels: dict[str, dict[str, list[dict]]] = {}

        for segments in window_results:
            local_to_global: dict[str, str] = {}
            by_local: dict[str, list[dict]] = {}
            for seg in segments:
                by_local.setdefault(seg["speaker"], []).append(seg)

            # Сначала пытаемся продолжить уже известного глобального спикера
            for local_label, segs in sorted(by_local.items()):
                best_label, best_score = None, 0.35
                for glabel, speakers_map in global_labels.items():
                    if local_label in local_to_global.values():
                        break
                    score = self._match_speaker_label(
                        local_label, glabel, segs,
                        [s for segs_g in speakers_map.values() for s in segs_g],
                    )
                    if score > best_score:
                        best_label, best_score = glabel, score
                if best_label is None:
                    used = set(local_to_global.values())
                    idx = len(global_labels)
                    while f"g{idx}" in used:
                        idx += 1
                    best_label = f"g{idx}"
                    global_labels[best_label] = {}
                local_to_global[local_label] = best_label
                global_labels[best_label][local_label] = segs

            for seg in segments:
                merged.append({
                    "speaker": local_to_global.get(seg["speaker"], seg["speaker"]),
                    "start": seg["start"],
                    "end": seg["end"],
                })

        merged.sort(key=lambda s: s["start"])
        # Склейка дублей и разделение перекрытий разных спикеров посередине
        resolved: list[dict] = []
        for seg in merged:
            if not resolved:
                resolved.append(dict(seg))
                continue
            last = resolved[-1]
            if seg["speaker"] == last["speaker"] and seg["start"] <= last["end"] + 1.0:
                last["end"] = max(last["end"], seg["end"])
                continue
            if seg["speaker"] != last["speaker"] and seg["start"] < last["end"]:
                boundary = (seg["start"] + min(seg["end"], last["end"])) / 2.0
                last["end"] = boundary
                seg["start"] = boundary
            if seg["end"] > seg["start"]:
                resolved.append(dict(seg))

        # Переименовываем глобальные ключи в читаемые номера по порядку появления
        rename: dict[str, int] = {}
        for seg in resolved:
            key = seg["speaker"]
            if key not in rename:
                rename[key] = len(rename) + 1
            seg["speaker"] = f"Спикер {rename[key]}"
        return resolved

    def _nemo_turns(self, input_file: str) -> list[dict]:
        """Диаризация через NVIDIA NeMo Sortformer (без транскрибации).

        Длинные записи обрабатываются окнами: у модели ограничение по
        длительности из-за памяти, а метки спикеров между окнами
        сопоставляются по перекрытию.
        """
        model = self._load_nemo_diarizer()
        waveform, sr = self._load_waveform(input_file, target_sr=16000, force_mono=True)
        if waveform.size == 0:
            raise RuntimeError("Аудио пустое")

        duration = waveform.shape[-1] / sr
        if self.expected_speakers is not None:
            self._log_progress(f"Параметры NeMo: ожидается спикеров={self.expected_speakers}")

        with torch.no_grad():
            if duration <= NEMO_MAX_WINDOW_SEC + NEMO_WINDOW_OVERLAP_SEC:
                raw = model.diarize(audio=input_file, batch_size=1)
                rows = raw[0] if isinstance(raw, (list, tuple)) and raw and isinstance(raw[0], (list, tuple)) else raw
                segments = self._parse_sortformer_segments(rows)
            else:
                # Оконная обработка длинных записей (NEMO_MAX_WINDOW_SEC).
                # NeMo 2.7.3 принимает на вход str-путь или np.ndarray (с
                # обязательным sample_rate), но НЕ dict-манифест — старый вариант
                # audio=[{...}] падает ValueError. Режем waveform на окна и
                # передаём numpy-срез с частотой дискретизации.
                stride = NEMO_MAX_WINDOW_SEC - NEMO_WINDOW_OVERLAP_SEC
                window_results = []
                offset = 0.0
                while offset < duration - 0.01:
                    win_dur = min(NEMO_MAX_WINDOW_SEC, duration - offset)
                    start = int(round(offset * sr))
                    end = int(min(round((offset + win_dur) * sr), waveform.shape[-1]))
                    chunk = waveform[start:end]
                    raw = model.diarize(
                        audio=chunk,
                        sample_rate=sr,
                        batch_size=1,
                    )
                    rows = raw[0] if isinstance(raw, (list, tuple)) and raw and isinstance(raw[0], (list, tuple)) else raw
                    window_results.append(self._parse_sortformer_segments(rows, offset_sec=offset))
                    offset += stride
                segments = self._merge_windowed_segments(window_results)

        if not segments:
            raise RuntimeError("NeMo не обнаружил активные участки речи")

        segments.sort(key=lambda s: s["start"])
        collapsed: list[dict] = []
        for seg in segments:
            if (
                collapsed
                and collapsed[-1]["speaker"] == seg["speaker"]
                and seg["start"] - collapsed[-1]["end"] < 1.0
            ):
                collapsed[-1]["end"] = max(collapsed[-1]["end"], seg["end"])
            else:
                collapsed.append(dict(seg))
        return self._normalize_speaker_segments(collapsed)

    def _diarize_with_nemo(self, input_file: str, output_file: str, append=False):
        segments = self._nemo_turns(input_file)
        return self._build_speaker_transcript(input_file, segments, output_file, append=append)

    def _check_vad_dependencies(self) -> bool:
        try:
            import diarize  # noqa: F401
            import torchaudio  # noqa: F401

            _patch_torchaudio_load()
            return True
        except (ImportError, OSError) as exc:
            self._log_progress(f"Диаризация недоступна: {exc}")
            return False

    def _ensure_vad_ready(self) -> bool:
        if not self._vad_check_done:
            self._vad_ready = self._check_vad_dependencies()
            self._vad_check_done = True
        return self._vad_ready

    def _is_silent(self, waveform: np.ndarray, threshold: float = 5e-4) -> bool:
        if waveform.size == 0:
            return True
        rms = float(np.sqrt(np.mean(np.square(waveform))))
        return rms < threshold

    def _save_silence_notice(self, output_file: str, append: bool = False):
        message = "Аудио не содержит различимого сигнала. Распознавание не выполнено.\n"
        mode = "a" if append and os.path.exists(output_file) else "w"
        with open(output_file, mode, encoding="utf-8") as f:
            f.write(message)
        return output_file

    def _compute_simple_features(self, chunk: np.ndarray) -> np.ndarray:
        if chunk.size == 0:
            return np.zeros(3, dtype=np.float32)
        energy = float(np.mean(np.abs(chunk)))
        sign_changes = np.diff(np.sign(chunk))
        zcr = float(np.mean(np.abs(sign_changes))) if sign_changes.size else 0.0
        diff = np.diff(chunk)
        spectral_flux = float(np.mean(np.abs(diff))) if diff.size else 0.0
        return np.array([energy, zcr, spectral_flux], dtype=np.float32)

    def _kmeans_labels(self, normalized: np.ndarray, k: int) -> np.ndarray:
        centers = normalized[np.linspace(0, normalized.shape[0] - 1, k, dtype=int)]
        labels = np.zeros(normalized.shape[0], dtype=np.int32)
        for _ in range(15):
            distances = np.linalg.norm(normalized[:, None, :] - centers[None, :, :], axis=2)
            labels = distances.argmin(axis=1)
            new_centers = []
            for idx in range(k):
                if np.any(labels == idx):
                    new_centers.append(normalized[labels == idx].mean(axis=0))
                else:
                    new_centers.append(centers[idx])
            new_centers = np.vstack(new_centers)
            if np.allclose(new_centers, centers):
                break
            centers = new_centers
        return labels

    def _silhouette_score(self, data: np.ndarray, labels: np.ndarray) -> float:
        unique = np.unique(labels)
        if unique.size < 2:
            return -1.0
        n = data.shape[0]
        if n < 3:
            return -1.0
        dmat = np.linalg.norm(data[:, None, :] - data[None, :, :], axis=2)
        total = 0.0
        for i in range(n):
            same = labels == labels[i]
            same[i] = False
            a = float(dmat[i, same].mean()) if np.any(same) else 0.0
            b = None
            for cluster in unique:
                if cluster == labels[i]:
                    continue
                mask = labels == cluster
                if not np.any(mask):
                    continue
                dist = float(dmat[i, mask].mean())
                b = dist if b is None else min(b, dist)
            if b is None:
                continue
            denom = max(a, b)
            score = (b - a) / denom if denom > 0 else 0.0
            total += score
        return total / n

    def _resolve_cluster_range(self, n_samples: int) -> tuple[int, int]:
        if n_samples <= 1:
            return 1, 1
        if self.expected_speakers is not None:
            k = max(1, min(self.expected_speakers, n_samples))
            return k, k
        min_k = self.min_speakers if self.min_speakers is not None else 1
        max_k = self.max_speakers if self.max_speakers is not None else min(6, n_samples)
        min_k = max(1, min(min_k, n_samples))
        max_k = max(min_k, min(max_k, n_samples))
        return min_k, max_k

    def _cluster_speakers(self, features: list[np.ndarray]) -> list[int]:
        if not features:
            return []
        data = np.vstack(features)
        if data.shape[0] == 1:
            return [1]
        data_mean = data.mean(axis=0)
        data_std = data.std(axis=0) + 1e-6
        normalized = (data - data_mean) / data_std
        min_k, max_k = self._resolve_cluster_range(normalized.shape[0])

        best_labels = None
        best_score = -2.0
        for k in range(min_k, max_k + 1):
            labels = self._kmeans_labels(normalized, k)
            score = self._silhouette_score(normalized, labels)
            if score > best_score:
                best_score = score
                best_labels = labels

        if best_labels is None:
            best_labels = self._kmeans_labels(normalized, min_k)

        mapping: dict[int, int] = {}
        speaker_ids: list[int] = []
        next_id = 1
        for lbl in best_labels:
            key = int(lbl)
            if key not in mapping:
                mapping[key] = next_id
                next_id += 1
            speaker_ids.append(mapping[key])
        return speaker_ids

    def _log_progress(self, message: str):
        text = f"[Transcriber] {message}"
        if self._log_callback:
            try:
                self._log_callback(text)
                return
            except Exception:
                pass
        print(text)

    def _prepare_audio_source(self, input_file: str):
        ext = os.path.splitext(input_file)[1].lower()
        if ext == ".wav":
            return input_file, None

        temp_dir = os.path.join(BASE_DIR, ".temp_audio", uuid.uuid4().hex)
        os.makedirs(temp_dir, exist_ok=True)
        temp_path = os.path.join(temp_dir, "source.wav")
        command = [
            ffmpeg_command(),
            "-y",
            "-i", input_file,
            "-ar", "16000",
            "-ac", "1",
            temp_path
        ]
        self._log_progress("Конвертация видео в аудио через ffmpeg")
        result = subprocess.run(command, capture_output=True)
        if result.returncode != 0:
            raise RuntimeError(f"ffmpeg ошибка {result.returncode}: {result.stderr.decode('utf-8', errors='ignore')}")
        return temp_path, temp_dir

    def _load_vosk_model(self):
        if self._vosk_model is not None:
            return True

        try:
            from vosk import Model, SetLogLevel
            SetLogLevel(-1)
            vosk_model_path = next((path for path in VOSK_MODEL_PATHS if os.path.isdir(path)), None)
            if vosk_model_path is None:
                raise FileNotFoundError("Vosk model directory not found")
            self._vosk_model = Model(vosk_model_path)
            return True
        except Exception as e:
            print(f"Ошибка загрузки Vosk: {e}")
            return False

    def _ensure_whisper_dependencies(self):
        global WhisperProcessor, WhisperForConditionalGeneration
        if WhisperProcessor is not None and WhisperForConditionalGeneration is not None:
            return True

        # Frozen AppImage must not use system dist-packages (wrong psutil ABI).
        if getattr(sys, "frozen", False):
            for entry in list(sys.path):
                norm = os.path.normpath(entry or "")
                if "dist-packages" in norm and ("/usr/lib/python" in norm or "/usr/local/lib/python" in norm):
                    try:
                        sys.path.remove(entry)
                    except ValueError:
                        pass
            for mod in list(sys.modules):
                if mod == "psutil" or mod.startswith("psutil."):
                    # Drop a previously injected system psutil before transformers import.
                    if getattr(sys.modules[mod], "__file__", None) and "/usr/lib/python" in str(
                        sys.modules[mod].__file__
                    ):
                        del sys.modules[mod]

        def _try_import():
            global WhisperProcessor, WhisperForConditionalGeneration
            from transformers import WhisperForConditionalGeneration as _WhisperForConditionalGeneration
            from transformers import WhisperProcessor as _WhisperProcessor
            WhisperProcessor = _WhisperProcessor
            WhisperForConditionalGeneration = _WhisperForConditionalGeneration
            return True

        try:
            return _try_import()
        except Exception as e:
            self._whisper_import_error = str(e)
            self._log_progress(f"Whisper недоступен: {e}")

        # Retry after dropping a broken project .venv transformers from path/cache.
        removed = False
        if LOCAL_SITE_PACKAGES in sys.path:
            try:
                sys.path.remove(LOCAL_SITE_PACKAGES)
                removed = True
            except ValueError:
                pass
        for mod in list(sys.modules):
            if mod == "transformers" or mod.startswith("transformers."):
                del sys.modules[mod]
        try:
            ok = _try_import()
            if ok:
                self._whisper_import_error = None
                self._log_progress("Whisper загружен из пользовательского окружения")
            return ok
        except Exception as e:
            self._whisper_import_error = str(e)
            self._log_progress(f"Whisper недоступен: {e}")
            return False
        finally:
            if removed and LOCAL_SITE_PACKAGES not in sys.path:
                sys.path.append(LOCAL_SITE_PACKAGES)

    def load_models(self):
        if self.model is not None and self.processor is not None:
            return True
        if not self._ensure_whisper_dependencies():
            return False

        preferred_names = [
            self.whisper_model_path,
            "whisper-medium",
            "whisper-small",
            "whisper-base",
            "whisper-tiny",
        ]
        seen = set()
        local_candidates = []
        for name in preferred_names:
            if not name or name in seen:
                continue
            seen.add(name)
            local_candidates.append((os.path.join(_MODEL_DIR, name), name))

        for local_path, model_name in local_candidates:
            if not os.path.isdir(local_path):
                continue
            try:
                self.processor = WhisperProcessor.from_pretrained(local_path)
                self.model = WhisperForConditionalGeneration.from_pretrained(local_path).to(self.device)
                self.model.config.forced_decoder_ids = None
                self.whisper_model_path = model_name
                return True
            except Exception:
                if self.model is not None:
                    del self.model
                    self.model = None
                if self.processor is not None:
                    del self.processor
                    self.processor = None

        whisper_models = [
            ("openai/whisper-medium", "whisper-medium"),
            ("openai/whisper-small", "whisper-small"),
            ("openai/whisper-base", "whisper-base"),
            ("openai/whisper-tiny", "whisper-tiny"),
        ]

        for model_id, model_name in whisper_models:
            try:
                self.processor = WhisperProcessor.from_pretrained(model_id)
                self.model = WhisperForConditionalGeneration.from_pretrained(model_id).to(self.device)
                self.model.config.forced_decoder_ids = None
                self.whisper_model_path = model_name
                return True
            except Exception:
                if self.model is not None:
                    del self.model
                    self.model = None
                if self.processor is not None:
                    del self.processor
                    self.processor = None
                continue

        return False

    def split_audio(self, input_file, segment_length=180, output_dir=".temp_segments"):
        os.makedirs(output_dir, exist_ok=True)

        if not os.path.exists(input_file):
            print(f"Ошибка разделения аудио: файл не найден {input_file}")
            return []

        file_size = os.path.getsize(input_file)
        if file_size < 1000:
            print(f"Ошибка разделения аудио: файл слишком мал ({file_size} байт)")
            return []

        try:
            waveform, sr = self._load_waveform(input_file, target_sr=None, force_mono=True)
        except Exception as e:
            print(f"Ошибка разделения аудио: {e}")
            return []

        if waveform.size == 0:
            print("Ошибка разделения аудио: длительность 0")
            return []

        segment_length_samples = int(segment_length * sr)
        if segment_length_samples <= 0:
            segment_length_samples = len(waveform)

        segments = []
        total_segments = max(1, int(math.ceil(len(waveform) / segment_length_samples)))
        for i, start in enumerate(range(0, len(waveform), segment_length_samples)):
            end = min(start + segment_length_samples, len(waveform))
            segment_path = os.path.join(output_dir, f"segment_{i}.wav")
            try:
                sf.write(segment_path, waveform[start:end], sr)
            except Exception as e:
                print(f"Ошибка сохранения сегмента {segment_path}: {e}")
                break
            segments.append({
                "path": segment_path,
                "start_time": start / sr,
                "end_time": end / sr
            })
            self._log_progress(f"Сегмент {i + 1}/{total_segments} подготовлен")

        return segments

    @staticmethod
    def _format_timecode(seconds: float) -> str:
        total = int(round(max(0.0, seconds)))
        hours, rem = divmod(total, 3600)
        minutes, secs = divmod(rem, 60)
        return f"{hours:02d}:{minutes:02d}:{secs:02d}"

    def save_results(self, transcripts, output_file, append=False, include_speaker_labels=True, include_timecodes=None):
        if not transcripts:
            return

        if include_timecodes is None:
            include_timecodes = self.include_timecodes

        mode = "a" if append else "w"

        with open(output_file, mode, encoding="utf-8") as f:
            if not append and include_speaker_labels:
                f.write("# Транскрипция\n\n")
            for item in sorted(transcripts, key=lambda x: x["start"]):
                text = item.get("text", "").strip()
                if not text:
                    continue
                parts = []
                if include_speaker_labels and item.get("speaker"):
                    parts.append(f"**{item['speaker']}**")
                if include_timecodes:
                    parts.append(f"[{self._format_timecode(item['start'])}]")
                prefix = f"{' '.join(parts)}: " if parts else ""
                f.write(f"{prefix}{text}\n\n")

    def save_speaker_segments(self, segments, output_file, append=False):
        mode = "a" if append and os.path.exists(output_file) else "w"
        with open(output_file, mode, encoding="utf-8") as f:
            if mode == "w":
                f.write("# Разбиение по спикерам\n\n")
            for item in sorted(segments, key=lambda x: x["start"]):
                start = str(timedelta(seconds=round(item["start"], 1)))
                end = str(timedelta(seconds=round(item["end"], 1)))
                speaker = item.get("speaker", "Спикер")
                f.write(f"**{speaker}** [{start} - {end}]\n\n")

    def cleanup(self, temp_dir):
        if temp_dir and os.path.isdir(temp_dir):
            shutil.rmtree(temp_dir, ignore_errors=True)

    def _get_safe_device(self):
        pref = self.compute_device or "auto"
        if pref == "cpu":
            self._log_progress("GPU отключено пользователем — CPU")
            return torch.device("cpu")
        if pref == "gpu" and not torch.cuda.is_available():
            self._log_progress("GPU: CUDA недоступна, используется CPU")
            return torch.device("cpu")
        if torch.cuda.is_available():
            device = torch.device("cuda")
            free, total = torch.cuda.mem_get_info(0)
            self._log_progress(
                f"GPU CUDA: {torch.cuda.get_device_name(0)}, "
                f"свободно {free / 2**30:.1f} ГБ из {total / 2**30:.1f} ГБ"
            )
            return device
        if hasattr(torch.backends, "mps") and torch.backends.mps.is_available():
            self._log_progress("GPU MPS (Apple Silicon) доступна")
            return torch.device("mps")
        self._log_progress("GPU недоступна, используется CPU")
        return torch.device("cpu")

    def _gpu_has_vram(self, required_gb: float, model_title: str) -> bool:
        """True, если CUDA доступна и свободного VRAM достаточно для модели."""
        if (self.compute_device or "auto") == "cpu":
            self._log_progress(f"{model_title}: GPU отключено пользователем, используется CPU")
            return False
        if not torch.cuda.is_available():
            self._log_progress(f"{model_title}: CUDA недоступна, используется CPU")
            return False
        try:
            free, total = torch.cuda.mem_get_info(0)
        except Exception:
            self._log_progress(f"{model_title}: не удалось определить видеопамять, используется CPU")
            return False
        free_gb = free / 2**30
        total_gb = total / 2**30
        if free_gb >= required_gb:
            self._log_progress(
                f"{model_title}: GPU — свободно {free_gb:.1f} ГБ из {total_gb:.1f} ГБ "
                f"(требуется ≥{required_gb:.0f} ГБ)"
            )
            return True
        self._log_progress(
            f"{model_title}: недостаточно видеопамяти "
            f"(свободно {free_gb:.1f} ГБ из {total_gb:.1f} ГБ, требуется ≥{required_gb:.0f} ГБ) — используется CPU"
        )
        return False

    def _load_waveform(self, input_file: str, target_sr: int | None = 16000, force_mono: bool = True):
        data, sr = sf.read(input_file)
        if data.ndim > 1 and force_mono:
            data = np.mean(data, axis=1)
        if target_sr is not None and sr != target_sr and sr > 0:
            gcd = math.gcd(sr, target_sr)
            up = target_sr // gcd
            down = sr // gcd
            data = signal.resample_poly(data, up, down)
            sr = target_sr
        return data.astype(np.float32), sr

    def full_transcribe(self, input_audio_file, output_file="meeting_transcript_final.txt", segment_length=180, append=False):
        """
        Основной метод: полный цикл обработки аудио.
        Возвращает путь к итоговому файлу.
        """
        print(f"[Transcriber] Начинаю транскрибацию: {input_audio_file}")

        if not os.path.exists(input_audio_file):
            raise RuntimeError(f"Аудио файл не найден: {input_audio_file}")

        prepared_audio, audio_temp_dir = self._prepare_audio_source(input_audio_file)
        temp_dir = os.path.join(BASE_DIR, ".temp_segments", uuid.uuid4().hex)

        file_size = os.path.getsize(prepared_audio)

        if file_size < 1000:
            raise RuntimeError(f"Аудио файл слишком мал: {file_size} байт")

        if self.use_whisperx:
            return self._transcribe_whisperx(prepared_audio, output_file, append=append)

        if self.diarization_method in ("diarize", "nemo"):
            return self._transcribe_speakers_only(prepared_audio, output_file, append=append)

        if self.use_vosk:
            return self._transcribe_vosk(prepared_audio, output_file)

        try:
            segments = self.split_audio(prepared_audio, segment_length=segment_length, output_dir=temp_dir)
            if not segments:
                raise RuntimeError("Ошибка при разбиении аудио")

            if not self.load_models():
                details = f": {self._whisper_import_error}" if self._whisper_import_error else ""
                self._log_progress(f"Whisper недоступен{details}")
                raise RuntimeError(f"Не удалось загрузить модели{details}")

            return self._transcribe_simple(prepared_audio, output_file, temp_dir, append=append)
        finally:
            self.cleanup(temp_dir)
            if audio_temp_dir:
                self.cleanup(audio_temp_dir)

    def _transcribe_speakers_only(self, input_file: str, output_file: str, append=False):
        if self.diarization_method == "diarize":
            if not self._ensure_vad_ready():
                raise RuntimeError("Модуль diarize недоступен")
            return self._diarize_with_diarize(input_file, output_file, append=append)

        if self.diarization_method == "nemo":
            if not self._ensure_nemo_ready():
                raise RuntimeError("NeMo недоступен")
            return self._diarize_with_nemo(input_file, output_file, append=append)

        raise RuntimeError("Метод разбиения по спикерам не выбран")

    def _diarize_with_diarize(self, input_file: str, output_file: str, append=False):
        import diarize

        kwargs = {}
        try:
            sig = inspect.signature(diarize.diarize)
            supported = set(sig.parameters.keys())
            if "num_speakers" in supported and self.expected_speakers is not None:
                kwargs["num_speakers"] = self.expected_speakers
            if "min_speakers" in supported and self.min_speakers is not None:
                kwargs["min_speakers"] = self.min_speakers
            if "max_speakers" in supported and self.max_speakers is not None:
                kwargs["max_speakers"] = self.max_speakers
        except Exception:
            kwargs = {}

        if kwargs:
            self._log_progress(f"Параметры diarize: {kwargs}")

        result = diarize.diarize(input_file, **kwargs)
        if not getattr(result, "segments", None):
            raise RuntimeError("diarize не вернул сегменты")

        segments = []
        for seg in result.segments:
            start = float(getattr(seg, "start", 0.0))
            end = float(getattr(seg, "end", 0.0))
            if end <= start:
                continue
            segments.append({
                "speaker": str(getattr(seg, "speaker", "Спикер")),
                "start": start,
                "end": end,
            })

        if not segments:
            raise RuntimeError("diarize не обнаружил активные сегменты")
        segments = self._normalize_speaker_segments(segments)
        return self._build_speaker_transcript(input_file, segments, output_file, append=append)

    def _ensure_whisperx_dependencies(self) -> bool:
        """Ленивый импорт faster-whisper (ядро WhisperX)."""
        if self._whisperx_model is not None:
            return True
        try:
            import faster_whisper  # noqa: F401
            return True
        except Exception as e:
            self._whisperx_import_error = str(e)
            self._log_progress(f"WhisperX недоступен: {e}")
            return False

    def _load_whisperx_model(self):
        """CTranslate2-модель: int8 на CPU, float16 на CUDA."""
        if self._whisperx_model is not None:
            return self._whisperx_model
        if not self._ensure_whisperx_dependencies():
            raise RuntimeError(
                f"Не удалось загрузить faster-whisper{': ' + self._whisperx_import_error if self._whisperx_import_error else ''}"
            )

        from faster_whisper import WhisperModel

        preferred = [
            os.path.join(_MODEL_DIR, "faster-whisper-large-v3-turbo"),
            getattr(sys, "_MEIPASS", None) and os.path.join(sys._MEIPASS, "faster-whisper-large-v3-turbo"),
            "large-v3-turbo",
        ]

        # Сначала пробуем GPU (если хватает видеопамяти), иначе CPU.
        use_gpu = self._gpu_has_vram(WHISPERX_VRAM_REQUIRED_GB, "WhisperX")
        attempts = []
        if use_gpu:
            attempts.append(("cuda", "float16"))
        attempts.append(("cpu", "int8"))

        last_error = None
        for device, compute_type in attempts:
            for candidate in preferred:
                if not candidate:
                    continue
                try:
                    self._log_progress(f"Загрузка WhisperX ({compute_type}): {candidate}")
                    self._whisperx_model = WhisperModel(
                        candidate,
                        device=device,
                        compute_type=compute_type,
                        download_root=_MODEL_DIR,
                    )
                    return self._whisperx_model
                except Exception as e:
                    last_error = e
                    self._whisperx_model = None
        raise RuntimeError(f"Не удалось загрузить модель WhisperX: {last_error}")

    def _transcribe_whisperx(self, input_file: str, output_file: str, append=False):
        """Полный конвейер в стиле WhisperX: ASR по всему файлу с VAD и
        пословными таймкодами + назначение спикеров словам по сегментам
        диаризации (pyannote/NeMo), затем сборка фраз."""
        model = self._load_whisperx_model()
        self._log_progress("Распознавание WhisperX (large-v3-turbo)...")

        segments_iter, info = model.transcribe(
            input_file,
            language="ru",
            task="transcribe",
            beam_size=5,
            word_timestamps=True,
            vad_filter=True,
            vad_parameters=dict(min_silence_duration_ms=500),
        )

        words = []
        asr_segments = []
        for seg in segments_iter:
            asr_segments.append({
                "start": float(seg.start),
                "end": float(seg.end),
                "text": (seg.text or "").strip(),
            })
            for w in getattr(seg, "words", None) or []:
                token = (w.word or "").strip()
                if not token:
                    continue
                words.append({
                    "word": token,
                    "start": float(w.start),
                    "end": float(w.end),
                })

        if not asr_segments and not words:
            raise RuntimeError("WhisperX не распознал речь в аудио")

        turns = []
        if self.diarization_method == "diarize":
            import diarize as diarize_module

            try:
                result = diarize_module.diarize(input_file)
            except Exception as exc:
                # Текст уже распознан — терять его из-за диаризации нельзя.
                self._log_progress(
                    f"Диаризация недоступна, текст без спикеров: {exc}"
                )
                result = None
            if result is not None:
                raw = []
                for seg in getattr(result, "segments", None) or []:
                    start = float(getattr(seg, "start", 0.0))
                    end = float(getattr(seg, "end", 0.0))
                    if end > start:
                        raw.append({
                            "speaker": str(getattr(seg, "speaker", "Спикер")),
                            "start": start,
                            "end": end,
                        })
                if raw:
                    turns = self._normalize_speaker_segments(raw)
        elif self.diarization_method == "nemo":
            try:
                turns = self._nemo_turns(input_file)
            except Exception as exc:
                # Текст уже распознан — терять его из-за диаризации нельзя.
                self._log_progress(
                    f"NeMo недоступен: разбиение по спикерам пропущено ({exc})"
                )

        if turns and not words:
            # Нет пословных таймкодов — назначаем спикеров сегментам целиком.
            transcripts = []
            for item in sorted(asr_segments, key=lambda x: x["start"]):
                speaker = self._speaker_for_interval(turns, item["start"], item["end"])
                if not speaker:
                    continue
                transcripts.append({**item, "speaker": speaker})
        elif turns:
            # Стиль WhisperX: спикер каждому слову по перекрытию, затем фразы.
            for w in words:
                w["speaker"] = self._speaker_for_interval(turns, w["start"], w["end"]) or ""
            transcripts = self._group_words_into_phrases(words)
        else:
            transcripts = [{**item} for item in sorted(asr_segments, key=lambda x: x["start"])]

        if not transcripts:
            raise RuntimeError("Не удалось сопоставить текст со спикерами")

        # Абзацы: реплики одного спикера склеиваем при паузе <1.2 c;
        # без спикеров новый абзац начинаем после тишины >=1.0 c.
        merge_gap = 1.2 if turns else 1.0
        merged = []
        for item in sorted(transcripts, key=lambda x: x["start"]):
            if (
                merged
                and merged[-1].get("speaker") == item.get("speaker")
                and item["start"] - merged[-1]["end"] < merge_gap
            ):
                merged[-1]["end"] = item["end"]
                merged[-1]["text"] = (merged[-1]["text"] + " " + item["text"]).strip()
            else:
                merged.append(dict(item))

        if turns:
            merged = self._merge_single_speaker_phrases(merged)

        self.save_results(
            merged,
            output_file,
            append=append,
            include_speaker_labels=bool(turns),
            include_timecodes=self.include_timecodes,
        )
        return output_file

    @staticmethod
    def _speaker_for_interval(turns: list[dict], start: float, end: float) -> str:
        """Спикер с максимальным перекрытием интервала [start, end]."""
        best_speaker = ""
        best_overlap = 0.0
        for t in turns:
            overlap = min(end, t["end"]) - max(start, t["start"])
            if overlap > best_overlap:
                best_overlap = overlap
                best_speaker = t["speaker"]
        return best_speaker

    def _group_words_into_phrases(self, words: list[dict]) -> list[dict]:
        phrases = []
        current = None
        for w in words:
            speaker = w.get("speaker") or ""
            if current and current["speaker"] == speaker and w["start"] - current["end"] <= 1.5:
                current["end"] = w["end"]
                current["text"] = (current["text"] + " " + w["word"]).strip()
            else:
                if current and current["text"]:
                    phrases.append(current)
                current = {
                    "speaker": speaker,
                    "start": w["start"],
                    "end": w["end"],
                    "text": w["word"],
                }
        if current and current["text"]:
            phrases.append(current)
        return phrases

    def _normalize_speaker_segments(self, segments: list[dict]) -> list[dict]:
        if not segments:
            return []

        ordered = sorted(
            (
                {
                    "speaker": str(item.get("speaker", "Спикер")),
                    "start": float(item.get("start", 0.0)),
                    "end": float(item.get("end", 0.0)),
                }
                for item in segments
            ),
            key=lambda x: x["start"],
        )

        merged = []
        for seg in ordered:
            if seg["end"] <= seg["start"]:
                continue
            if merged and merged[-1]["speaker"] == seg["speaker"] and seg["start"] - merged[-1]["end"] <= 0.5:
                merged[-1]["end"] = max(merged[-1]["end"], seg["end"])
            else:
                merged.append(seg)

        if len(merged) > 1:
            for idx in range(len(merged)):
                dur = merged[idx]["end"] - merged[idx]["start"]
                if dur >= 0.8:
                    continue
                prev_seg = merged[idx - 1] if idx > 0 else None
                next_seg = merged[idx + 1] if idx + 1 < len(merged) else None
                if prev_seg and next_seg and prev_seg["speaker"] == next_seg["speaker"]:
                    merged[idx]["speaker"] = prev_seg["speaker"]

        durations = {}
        total = 0.0
        for seg in merged:
            dur = max(0.0, seg["end"] - seg["start"])
            durations[seg["speaker"]] = durations.get(seg["speaker"], 0.0) + dur
            total += dur

        if not durations or total <= 0:
            return merged

        dominant = max(durations, key=durations.get)
        dominant_share = durations[dominant] / total

        expected = self.expected_speakers
        min_s = self.min_speakers
        max_s = self.max_speakers

        if expected == 1:
            for seg in merged:
                seg["speaker"] = dominant
        else:
            if dominant_share >= 0.82 and len(durations) > 2 and expected in (None, 1):
                for seg in merged:
                    seg["speaker"] = dominant
            for seg in merged:
                share = durations.get(seg["speaker"], 0.0) / total
                if share < 0.04 and expected in (None, 1, 2):
                    seg["speaker"] = dominant

            unique_labels = list(dict.fromkeys(seg["speaker"] for seg in merged))
            if expected is not None and len(unique_labels) > expected:
                by_duration = sorted(unique_labels, key=lambda s: durations.get(s, 0.0), reverse=True)
                keep = set(by_duration[:expected])
                for seg in merged:
                    if seg["speaker"] not in keep:
                        seg["speaker"] = dominant
            elif max_s is not None and len(unique_labels) > max_s:
                by_duration = sorted(unique_labels, key=lambda s: durations.get(s, 0.0), reverse=True)
                keep = set(by_duration[:max_s])
                for seg in merged:
                    if seg["speaker"] not in keep:
                        seg["speaker"] = dominant

            if min_s is not None:
                unique_after = list(dict.fromkeys(seg["speaker"] for seg in merged))
                if len(unique_after) < min_s:
                    self._log_progress(f"Предупреждение: найдено {len(unique_after)} спикеров при min={min_s}")

        compact = []
        for seg in merged:
            if compact and compact[-1]["speaker"] == seg["speaker"] and seg["start"] - compact[-1]["end"] <= 0.8:
                compact[-1]["end"] = max(compact[-1]["end"], seg["end"])
            else:
                compact.append(seg)

        speaker_order = {}
        idx = 1
        for seg in compact:
            key = seg["speaker"]
            if key not in speaker_order:
                speaker_order[key] = idx
                idx += 1
            seg["speaker"] = f"Спикер {speaker_order[key]}"

        return compact

    def _transcribe_chunk_vosk(self, chunk: np.ndarray, sr: int) -> str:
        if not self._load_vosk_model():
            raise RuntimeError("Не удалось загрузить модель Vosk")
        try:
            from vosk import KaldiRecognizer
        except Exception as e:
            raise RuntimeError(f"Не удалось загрузить Vosk: {e}")

        if sr != 16000 and sr > 0:
            gcd = math.gcd(sr, 16000)
            up = 16000 // gcd
            down = sr // gcd
            chunk = signal.resample_poly(chunk, up, down)
            sr = 16000

        pcm = np.clip(chunk, -1.0, 1.0)
        pcm = (pcm * 32767.0).astype(np.int16).tobytes()
        rec = KaldiRecognizer(self._vosk_model, sr)
        rec.SetWords(False)
        rec.AcceptWaveform(pcm)
        result = json.loads(rec.FinalResult())
        return result.get("text", "").strip()

    def _transcribe_chunk_whisper(self, chunk: np.ndarray, sr: int) -> str:
        if not self.load_models():
            details = f": {self._whisper_import_error}" if self._whisper_import_error else ""
            raise RuntimeError(f"Whisper недоступен{details}")

        inputs = self.processor(
            chunk,
            sampling_rate=sr,
            return_tensors="pt"
        ).input_features.to(self.device)

        with torch.no_grad():
            predicted_ids = self.model.generate(
                inputs,
                max_new_tokens=220,
                language="russian",
                task="transcribe",
                use_cache=True,
            )

        text = self.processor.batch_decode(predicted_ids, skip_special_tokens=True)[0]
        return text.strip()

    def _build_speaker_transcript(self, input_file: str, segments: list[dict], output_file: str, append=False):
        waveform, sr = self._load_waveform(input_file, target_sr=16000, force_mono=True)
        if waveform.size == 0:
            raise RuntimeError("Аудио пустое")

        use_whisper_backend = self.load_models()
        if use_whisper_backend:
            self._log_progress("Для текста по спикерам используем Whisper")
        else:
            use_vosk_backend = self._load_vosk_model()
            if use_vosk_backend:
                self._log_progress("Whisper недоступен, используем Vosk для текста по спикерам")
            else:
                raise RuntimeError("Нет доступной ASR модели для текста по спикерам")

        transcripts = []
        total = len(segments)
        for idx, seg in enumerate(segments):
            start = max(0.0, float(seg["start"]))
            end = max(start, float(seg["end"]))
            start_i = int(start * sr)
            end_i = int(end * sr)
            if end_i - start_i < int(0.4 * sr):
                continue

            chunk = waveform[start_i:end_i]
            if self._is_silent(chunk):
                continue

            try:
                if use_whisper_backend:
                    text = self._transcribe_chunk_whisper(chunk, sr)
                else:
                    text = self._transcribe_chunk_vosk(chunk, sr)
            except Exception:
                continue

            if not text:
                continue

            transcripts.append({
                "speaker": seg.get("speaker", "Спикер"),
                "start": start,
                "end": end,
                "text": text,
            })
            self._log_progress(f"Спикер-сегмент {idx + 1}/{total} обработан")

        if not transcripts:
            raise RuntimeError("Не удалось получить текст по сегментам спикеров")

        merged = []
        for item in sorted(transcripts, key=lambda x: x["start"]):
            if merged and merged[-1]["speaker"] == item["speaker"] and item["start"] - merged[-1]["end"] < 1.2:
                merged[-1]["end"] = item["end"]
                merged[-1]["text"] = (merged[-1]["text"] + " " + item["text"]).strip()
            else:
                merged.append(item)

        merged = self._merge_single_speaker_phrases(merged)

        self.save_results(
            merged,
            output_file,
            append=append,
            include_speaker_labels=True,
            include_timecodes=self.include_timecodes,
        )
        return output_file

    def _merge_single_speaker_phrases(self, transcripts: list[dict]) -> list[dict]:
        if not transcripts:
            return transcripts

        speakers = list({item.get("speaker", "Спикер") for item in transcripts})
        if self.expected_speakers == 1 or len(speakers) == 1:
            merged = []
            for item in sorted(transcripts, key=lambda x: x["start"]):
                if not merged:
                    merged.append(dict(item))
                    continue

                prev = merged[-1]
                gap = float(item["start"]) - float(prev["end"])
                prev_text_len = len(prev.get("text", ""))
                # Для одного спикера склеиваем крупнее, разделяя только по заметным паузам.
                should_split = gap > 2.4 or (gap > 1.0 and prev_text_len > 260)

                if should_split:
                    merged.append(dict(item))
                else:
                    prev["end"] = item["end"]
                    prev["text"] = (prev.get("text", "") + " " + item.get("text", "")).strip()
            return merged

        return transcripts

    def _transcribe_vosk(self, input_file: str, output_file: str):
        """Транскрибация через Vosk."""
        if not self._load_vosk_model():
            raise RuntimeError("Не удалось загрузить модель Vosk")

        try:
            from vosk import KaldiRecognizer
        except Exception as e:
            raise RuntimeError(f"Не удалось загрузить Vosk: {e}")

        wav_path = input_file.replace('.wav', '_vosk.wav')
        subprocess.run([ffmpeg_command(), '-y', '-i', input_file, '-ar', '16000', '-ac', '1', '-codec:a', 'pcm_s16le', wav_path],
                    capture_output=True)

        wf = wave.open(wav_path, 'rb')
        if wf.getnchannels() != 1 or wf.getsampwidth() != 2 or wf.getcomptype() != "NONE":
            raise RuntimeError("Audio file must be WAV format mono PCM")

        rec = KaldiRecognizer(self._vosk_model, wf.getframerate())
        rec.SetWords(True)

        transcripts = []

        def _handle_result(raw_json: str):
            try:
                result = json.loads(raw_json)
            except Exception:
                return
            words = result.get("result") or []
            text = (result.get("text") or "").strip()
            if not text:
                return
            if words:
                start = float(words[0].get("start", 0.0))
                end = float(words[-1].get("end", 0.0))
            else:
                start = end = 0.0
            transcripts.append({
                "speaker": "",
                "start": start,
                "end": end,
                "text": text,
            })

        while True:
            data = wf.readframes(4000)
            if len(data) == 0:
                break
            if rec.AcceptWaveform(data):
                _handle_result(rec.Result())

        _handle_result(rec.FinalResult())

        wf.close()

        if not transcripts:
            raise RuntimeError("Vosk не распознал текст")

        # Абзацы: разрыв >=1 c между репликами = новая тишина.
        merged = []
        for item in sorted(transcripts, key=lambda x: x["start"]):
            if (
                merged
                and item["start"] - merged[-1]["end"] < 1.0
            ):
                merged[-1]["end"] = item["end"]
                merged[-1]["text"] = (merged[-1]["text"] + " " + item["text"]).strip()
            else:
                merged.append(dict(item))

        self.save_results(
            merged,
            output_file,
            append=False,
            include_speaker_labels=False,
            include_timecodes=self.include_timecodes,
        )
        return output_file

    def _transcribe_simple(self, input_file: str, output_file: str, temp_dir: str, append=False):
        """Транскрибация - используем текстовый режим или diarize."""
        if self.diarization_method == "diarize":
            if self._ensure_vad_ready():
                return self._transcribe_with_diarization(input_file, output_file, temp_dir, append)
            self._log_progress("Диаризация отключена: не удалось загрузить torchaudio/diarize")
            return self._transcribe_text_only(input_file, output_file, temp_dir, append, assign_speakers=False)
        if self.diarization_method == "nemo":
            if self._check_nemo_dependencies() and self._ensure_nemo_ready():
                return self._transcribe_text_only(input_file, output_file, temp_dir, append, assign_speakers=True)
            self._log_progress("NeMo недоступен: разбиение по спикерам пропущено")
            return self._transcribe_text_only(input_file, output_file, temp_dir, append, assign_speakers=False)
        return self._transcribe_text_only(input_file, output_file, temp_dir, append, assign_speakers=False)

    def _transcribe_text_only(self, input_file: str, output_file: str, temp_dir: str, append=False, assign_speakers=False):
        """Простая транскрибация без диаризации."""
        waveform, sr = self._load_waveform(input_file, target_sr=16000, force_mono=True)

        if len(waveform) == 0:
            raise RuntimeError("Аудио пустое")
        if self._is_silent(waveform):
            self._log_progress("Сигнал слишком тихий, распознавание пропущено")
            return self._save_silence_notice(output_file, append)

        chunk_duration = 15
        chunk_samples = int(chunk_duration * sr)

        all_texts = []

        total_chunks = max(1, math.ceil(len(waveform) / chunk_samples))
        features = []
        for idx, start in enumerate(range(0, len(waveform), chunk_samples)):
            end = min(start + chunk_samples, len(waveform))
            chunk = waveform[start:end]
            if self._is_silent(chunk):
                continue

            inputs = self.processor(
                chunk,
                sampling_rate=sr,
                return_tensors="pt"
            ).input_features.to(self.device)

            with torch.no_grad():
                predicted_ids = self.model.generate(
                    inputs,
                    max_new_tokens=300,
                    language="russian",
                    task="transcribe",
                    use_cache=True
                )

            text = self.processor.batch_decode(predicted_ids, skip_special_tokens=True)[0]
            end_time = end / sr
            all_texts.append({
                "speaker": "Спикер",
                "start": start / sr,
                "end": end_time,
                "text": text
            })
            self._log_progress(f"Транскрибирован блок {idx + 1}/{total_chunks}")
            if assign_speakers:
                features.append(self._compute_simple_features(chunk))

        if assign_speakers and len(all_texts) >= 1:
            self._log_progress("Применяем упрощённую кластеризацию по спикерам")
            labels = self._cluster_speakers(features)
            for transcript, label in zip(all_texts, labels):
                transcript["speaker"] = f"Спикер {label}"

        if not all_texts:
            return self._save_silence_notice(output_file, append)

        self.save_results(
            all_texts,
            output_file,
            append=append,
            include_speaker_labels=assign_speakers,
            include_timecodes=assign_speakers
        )
        return output_file

    def _transcribe_with_diarization(self, input_file: str, output_file: str, temp_dir: str, append=False):
        """Транскрибация с диаризацией спикеров (использует diarize)."""
        import diarize

        try:
            result = diarize.diarize(input_file)
        except Exception as exc:
            self._log_progress(f"Ошибка diarize: {exc}")
            return self._transcribe_text_only(input_file, output_file, temp_dir, append, assign_speakers=True)

        try:
            waveform, sr = self._load_waveform(input_file, target_sr=16000, force_mono=True)
            if len(waveform) == 0:
                raise RuntimeError("Аудио пустое")
            if self._is_silent(waveform):
                self._log_progress("Сигнал слишком тихий для диаризации, пропускаем распознавание")
                return self._save_silence_notice(output_file, append)
        except Exception as exc:
            self._log_progress(f"Ошибка загрузки аудио для диаризации: {exc}")
            return self._transcribe_text_only(input_file, output_file, temp_dir, append, assign_speakers=True)

        unique_speakers = list(dict.fromkeys(seg.speaker for seg in result.segments))
        use_diarization = len(unique_speakers) <= 2

        if use_diarization:
            sorted_segs = sorted(result.segments, key=lambda x: x.start)

            if len(sorted_segs) < 2:
                use_diarization = False
            else:
                max_gap = 0
                split_time = 0
                for i in range(len(sorted_segs) - 1):
                    gap = sorted_segs[i + 1].start - sorted_segs[i].end
                    if gap > max_gap:
                        max_gap = gap
                        split_time = (sorted_segs[i].end + sorted_segs[i + 1].start) / 2

                if max_gap < 1.0:
                    use_diarization = False

        all_transcripts = []
        segments_count = len(result.segments)
        for index, segment in enumerate(result.segments):
            start_sample = int(segment.start * sr)
            end_sample = int(segment.end * sr)
            if start_sample >= end_sample:
                continue

            audio_chunk = waveform[start_sample:end_sample]
            if len(audio_chunk) < 1600:
                continue

            if use_diarization:
                seg_mid = (segment.start + segment.end) / 2
                if seg_mid < split_time:
                    label = "Спикер 1"
                else:
                    label = "Спикер 2"
            else:
                label = "Спикер"

            try:
                inputs = self.processor(
                    audio_chunk,
                    sampling_rate=sr,
                    return_tensors="pt"
                ).input_features.to(self.device)

                with torch.no_grad():
                    predicted_ids = self.model.generate(
                        inputs,
                        max_new_tokens=300,
                        language="russian",
                        task="transcribe"
                    )

                text = self.processor.batch_decode(predicted_ids, skip_special_tokens=True)[0]

                all_transcripts.append({
                    "speaker": label,
                    "start": segment.start,
                    "end": segment.end,
                    "text": text.strip()
                })
            except Exception:
                continue
            self._log_progress(f"Диаризация сегмента {index + 1}/{segments_count} завершена")

        if not all_transcripts:
            self._log_progress("Диаризация не дала результатов, переходим к обычной транскрипции")
            return self._transcribe_text_only(input_file, output_file, temp_dir, append, assign_speakers=True)

        all_transcripts.sort(key=lambda x: x["start"])
        merged = []
        for seg in all_transcripts:
            if merged and merged[-1]["speaker"] == seg["speaker"] and seg["start"] - merged[-1]["end"] < 2.0:
                merged[-1]["end"] = seg["end"]
                merged[-1]["text"] = merged[-1]["text"] + " " + seg["text"]
            else:
                merged.append(seg)

        self.save_results(merged, output_file, append=append)
        self.cleanup(temp_dir)
        return output_file

















"""
### Описание класса AudioTranscriberService

Файл `audio_transcriber_service.py` содержит класс **AudioTranscriberService**, который инкапсулирует полный цикл обработки аудиофайлов для:
- разбиения на сегменты,
- выполнения диаризации (определения участников речи),
- транскрибации (распознавания текста с помощью Whisper),
- сохранения результата с таймкодами и спикерами.

Класс ориентирован на **простое использование как в обычном скрипте, так и внутри других модулей или API**.

---

#### Основные возможности класса:

- **split_audio:** разбивает длинные WAV-файлы на маленькие сегменты (например, по 3 минуты).
- **load_models:** загружает модели для диаризации и ASR, автоматически выбирает подходящие (умеет падать на меньшие, если памяти не хватает).
- **process_diarization:** делит аудио на фрагменты с разными спикерами (speaker diarization).
- **transcribe_segments:** для каждого фрагмента получает текст.
- **save_results:** сохраняет результаты с таймкодами и метками спикеров.
- **cleanup:** удаляет временные файлы.
- **full_transcribe:** ОДИН метод, который делает всё вышеописанное, от исходного файла до результата.

---

#### Пример использования

Вот минимальный скрипт, который показывает, как импортировать и применить этот класс для обработки файла:

```python example_usage.py
from audio_transcriber_service import AudioTranscriberService

# 1. Укажите ваш Hugging Face токен (для доступа к модели pyannote)
HF_TOKEN = "hf_xxxxxxxxxxxxxxxxxxxxxxxxxxxxx"             # <-- Замените на свой токен

# 2. Укажите путь к модели Whisper (должна быть скачана заранее)
WHISPER_MODEL_PATH = "whisper-medium"                     # Например, "whisper-medium", "whisper-small" и т.п.

# 3. Входной аудиофайл (формат WAV, 16kHz, mono желательно)
AUDIO_FILE = "path/to/your/audio.wav"                     # Замените на путь к своему файлу

# 4. Итоговый текстовый файл (где будет результат)
OUTPUT_TXT = "meeting_transcript_final.txt"

# 5. Создать объект сервиса
transcriber = AudioTranscriberService(
    auth_token=HF_TOKEN,
    whisper_model_path=WHISPER_MODEL_PATH
)

# 6. Вызвать полный процесс транскрибации (результат будет сохранён в OUTPUT_TXT)
try:
    result_file = transcriber.full_transcribe(
        input_audio_file=AUDIO_FILE,
        output_file=OUTPUT_TXT,
        segment_length=180           # можно указать меньше/больше, если аудио особое
    )
    print(f"Транскрипция завершена. Результат: {result_file}")
except Exception as e:
    print(f"Ошибка обработки: {e}")
```

---

#### Что делает этот пример:
1. **Импортирует** класс.
2. **Создаёт** объект с нужными параметрами.
3. **Выполняет** полный цикл обработки аудиофайла одной командой (`full_transcribe`, см. документацию в коде).
4. **Результат** ― файл с таймкодами, текстом и идентифицированным спикером для каждого фрагмента.


#### Примечания:
- Модели Whisper и pyannote должны быть **предварительно скачаны** (см. dawnload_model.py/инструкции ниже).
- Требует достаточно памяти на GPU, если есть; иначе ― занимает намного дольше (работает на CPU).
- Поддержка форматов ― WAV (лучше 1 канал, 16kHz).
- В случае большой продолжительности автоматически разбивает файл на части.

---

#### Структура результата (формат выводимого txt):

```
[0:00:00 - 0:03:00] SPEAKER_0:
Добро пожаловать на заседание...

[0:03:00 - 0:06:00] SPEAKER_1:
Спасибо, следующий вопрос...
```

---

#### Как получить модели

Используйте ваш `dawnload_model.py` или вручную скачайте по инструкции (см. комментарии в вашем оригинале).

---
"""
