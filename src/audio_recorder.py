import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
import wave

from ffmpeg_locator import ffmpeg_command

_CREATIONFLAGS_NO_WINDOW = 0x08000000 if sys.platform == "win32" else 0


class AudioRecorder:
    """Записывает аудио со всех источников (микрофон и системный звук) в WAV через ffmpeg."""
    def __init__(self, filename="audio.wav", segment_length=120, segment_dir=".temp_segments"):
        """Инициализирует рекордер с настройками выходного файла и каталогом сегментов."""
        self.filename = filename
        self.process_main = None
        self.process_system = None
        self.process_mic = None
        self.process_segments = None
        self.segment_length = segment_length
        self.segment_dir = segment_dir
        self.sources = []
        self.default_source = None
        self.default_monitor = None
        self.default_sink = None
        self._pw_links = []
        self._pw_record_proc = None
        self.mic_temp = os.path.join(self.segment_dir, "_mic.wav")
        self.audio_start_time = None
        self.audio_capture_time = None
        self.audio_silence_duration = None
        self.is_windows = sys.platform == "win32"
        self.is_macos = sys.platform == "darwin"
        self._macos_audio_devices = {}
        self.ffmpeg_bin = ffmpeg_command()
        self._loopback_stop = threading.Event()
        self._loopback_thread = None
        self._refresh_sources()
        if not os.path.exists(self.segment_dir):
            os.makedirs(self.segment_dir, exist_ok=True)

    @staticmethod
    def _popen(cmd, **kwargs):
        """subprocess.Popen без видимого консольного окна на Windows."""
        kwargs.setdefault("stdout", subprocess.DEVNULL)
        kwargs.setdefault("stderr", subprocess.DEVNULL)
        if _CREATIONFLAGS_NO_WINDOW:
            kwargs.setdefault("creationflags", _CREATIONFLAGS_NO_WINDOW)
        return subprocess.Popen(cmd, **kwargs)

    def _pw_dump(self):
        """JSON-дамп графа PipeWire или None при ошибке."""
        try:
            out = subprocess.check_output(["pw-dump"], stderr=subprocess.DEVNULL, timeout=10)
            return json.loads(out)
        except Exception:
            return None

    def _pw_defaults(self):
        """Имена дефолтных sink/source из метаданных PipeWire (без pactl)."""
        dump = self._pw_dump()
        if not dump:
            return None, None
        sink = source = None
        for obj in dump:
            props = obj.get("props", {}) or {}
            if str(props.get("metadata.name")) != "default":
                continue
            for entry in obj.get("metadata", []) or []:
                key = entry.get("key")
                value = entry.get("value")
                if isinstance(value, dict):
                    value = value.get("name")
                if key == "default.audio.sink":
                    sink = value
                elif key == "default.audio.source":
                    source = value
        return sink, source

    def _check_pipewire(self):
        """Проверяет, работает ли PipeWire, через pactl или pw-dump."""
        try:
            out = subprocess.check_output(
                ["pactl", "info"],
                stderr=subprocess.DEVNULL, timeout=3
            ).decode()
            return "PipeWire" in out or "pipewire" in out
        except Exception:
            pass
        try:
            out = subprocess.check_output(
                ["pw-dump", "--version"],
                stderr=subprocess.DEVNULL, timeout=3
            ).decode()
            return "pipewire" in out.lower()
        except Exception:
            return False

    def _find_all_sources(self):
        """Возвращает список всех доступных аудио-источников для текущей платформы."""
        if self.is_windows:
            return self._find_windows_sources()
        if self.is_macos:
            return self._find_macos_sources()

        sources = []
        try:
            out = subprocess.check_output(['pactl', 'list', 'sources', 'short'], timeout=5).decode()
            for line in out.splitlines():
                parts = line.split()
                if len(parts) >= 2:
                    sources.append(parts[1])
        except Exception:
            pass
        if not sources:
            # pactl отсутствует — строим список из графа PipeWire:
            # физические микрофоны + псевдо-источники <sink>.monitor
            dump = self._pw_dump()
            sinks = []
            for obj in dump or []:
                props = (obj.get("info", {}) or {}).get("props", {}) or {}
                cls = props.get("media.class")
                name = props.get("node.name")
                if not name or name in sources:
                    continue
                if cls == "Audio/Source":
                    sources.append(name)
                elif cls == "Audio/Sink":
                    sinks.append(name)
            for sink_name in sinks:
                monitor = f"{sink_name}.monitor"
                if monitor not in sources:
                    sources.append(monitor)
        if not sources:
            sources = ["default"]
        return sources

    def _find_windows_sources(self):
        """Список аудио-устройств Windows (dshow) или 'default' при их отсутствии."""
        devices = self._list_windows_dshow_devices()
        if devices:
            return devices
        return ["default"]

    def _refresh_sources(self):
        """Обновляет self.sources и определяет дефолтные микрофон и системный источник."""
        sources = self._find_all_sources()
        if self.is_windows:
            default_monitor = self._pick_windows_system_device(sources)
            default_source = self._pick_windows_mic_device(sources, default_monitor)
            if default_monitor and default_monitor not in sources:
                sources.insert(0, default_monitor)
            if default_source and default_source not in sources:
                sources.insert(0, default_source)
            self.sources = sources
            self.default_source = default_source
            self.default_monitor = default_monitor
            self.default_sink = None
            return

        if self.is_macos:
            system_name = None
            mic_name = None
            for s in sources:
                if s != "default" and self._is_macos_system_device(s):
                    system_name = s
                    break
            for s in sources:
                if s == "default":
                    continue
                if s == system_name:
                    continue
                if not self._is_macos_system_device(s):
                    mic_name = s
                    break
            self.sources = sources
            self.default_source = mic_name
            self.default_monitor = system_name
            self.default_sink = None
            return

        default_sink = self._detect_default_sink()
        default_source = self._detect_default_source()
        default_monitor = f"{default_sink}.monitor" if default_sink else None
        if default_source and default_source not in sources:
            sources.insert(0, default_source)
        if default_monitor and default_monitor not in sources:
            sources.insert(0, default_monitor)
        self.sources = sources
        self.default_source = default_source
        self.default_monitor = default_monitor
        self.default_sink = default_sink

    def _list_macos_avfoundation_devices(self):
        """Список аудио-устройств AVFoundation как [(index, name), ...]."""
        try:
            proc = subprocess.run(
                [self.ffmpeg_bin, "-hide_banner", "-list_devices", "true", "-f", "avfoundation", "-i", ""],
                capture_output=True,
                text=True,
                check=False,
                timeout=10,
            )
            output = (proc.stdout or "") + "\n" + (proc.stderr or "")
        except Exception:
            return []

        devices = []
        in_audio = False
        for raw_line in output.splitlines():
            line = raw_line.strip()
            low = line.lower()
            if "video devices" in low:
                in_audio = False
                continue
            if "audio devices" in low:
                in_audio = True
                continue
            if not in_audio:
                continue
            m = re.search(r"\[\s*(\d+)\]\s*(.+)$", line)
            if not m:
                continue
            try:
                idx = int(m.group(1))
            except ValueError:
                continue
            name = m.group(2).strip()
            if not name:
                continue
            devices.append((idx, name))
        return devices

    def _find_macos_sources(self):
        """Список аудио-устройств AVFoundation или 'default' при их отсутствии."""
        devices = self._list_macos_avfoundation_devices()
        if devices:
            self._macos_audio_devices = {name: idx for idx, name in devices}
            return [name for _, name in devices]
        self._macos_audio_devices = {}
        return ["default"]

    @staticmethod
    def _is_macos_system_device(name):
        """Видит ли устройство системный звук (loopback-дрова, напр. BlackHole)."""
        low = (name or "").lower()
        markers = (
            "blackhole", "soundflower", "loopback", "background music",
            "virtual", "aggregate", "ishowu",
        )
        return any(m in low for m in markers)

    def _macos_mic_device_names(self):
        """Имена микрофонов macOS (все источники, кроме системных и default)."""
        return [s for s in self.sources if s != "default" and not self._is_macos_system_device(s)]

    def _macos_input_spec(self, device_name):
        """AVFoundation-спецификация входа `:audio` (только звук, без видео)."""
        if not device_name or device_name == "default":
            return ":default"
        idx = self._macos_audio_devices.get(device_name)
        if idx is not None:
            return f":{idx}"
        return f":{device_name}"

    def _start_macos_capture(self, device_name, out_path, key="main"):
        """Запуск записи устройства AVFoundation в out_path. key: main/system/mic."""
        try:
            proc = self._popen([
                self.ffmpeg_bin, "-y",
                "-f", "avfoundation", "-i", self._macos_input_spec(device_name),
                "-ar", "48000", "-ac", "2", out_path,
            ])
        except Exception:
            return False
        time.sleep(0.4)
        if proc.poll() is None:
            if key == "mic":
                self.process_mic = proc
            elif key == "system":
                self.process_system = proc
                self.process_main = proc
            else:
                self.process_main = proc
            return True
        try:
            proc.terminate()
        except Exception:
            pass
        return False

    def _start_macos_default(self):
        """Запускает запись дефолтного устройства AVFoundation в self.filename."""
        try:
            proc = self._popen([
                self.ffmpeg_bin, "-y",
                "-f", "avfoundation", "-i", ":default",
                "-ar", "48000", "-ac", "2", self.filename,
            ])
        except Exception:
            return False
        time.sleep(0.4)
        if proc.poll() is None:
            self.process_main = proc
            return True
        try:
            proc.terminate()
        except Exception:
            pass
        return False

    def _start_macos_all_sources(self):
        """Системный звук (loopback-устройство) + микрофон на macOS."""
        self._prepare_files()
        self._refresh_sources()
        system_name = self.default_monitor
        mic_names = self._macos_mic_device_names()
        mic_name = mic_names[0] if mic_names else None
        if system_name:
            if self._start_macos_capture(system_name, self.filename, key="system"):
                if mic_name and mic_name != system_name:
                    self._start_macos_capture(mic_name, self.mic_temp, key="mic")
                return
        if mic_name:
            if self._start_macos_capture(mic_name, self.filename, key="main"):
                return
        self._start_macos_default()

    def _start_macos_mic_only(self):
        """Запускает запись только микрофона на macOS."""
        self._prepare_files()
        self._refresh_sources()
        mic_names = self._macos_mic_device_names()
        mic = mic_names[0] if mic_names else (self.default_source or "default")
        self._start_macos_capture(mic, self.filename, key="main")

    def _start_macos_system_only(self):
        """Запускает запись только системного звука на macOS."""
        self._prepare_files()
        self._refresh_sources()
        system_name = self.default_monitor
        if system_name:
            self._start_macos_capture(system_name, self.filename, key="system")
            return
        self._start_macos_default()

    def _list_windows_dshow_devices(self):
        """Список аудио-устройств DirectShow через ffmpeg -list_devices."""
        try:
            proc = subprocess.run(
                [self.ffmpeg_bin, "-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"],
                capture_output=True,
                text=True,
                check=False,
                timeout=10,
            )
            output = (proc.stdout or "") + "\n" + (proc.stderr or "")
        except Exception:
            return []

        devices = []
        in_audio_block = False
        has_header = False
        for raw_line in output.splitlines():
            line = raw_line.strip()
            low = line.lower()
            if "directshow audio devices" in low:
                in_audio_block = True
                has_header = True
                continue
            if "directshow video devices" in low:
                in_audio_block = False
                continue
            if has_header and not in_audio_block:
                continue

            first = line.find('"')
            if first == -1:
                continue
            second = line.find('"', first + 1)
            if second == -1:
                continue
            name = line[first + 1:second].strip()
            if not name:
                continue

            # Формат gyan.dev: устройства идут после заголовка
            # "DirectShow audio devices"
            if has_header:
                if name not in devices:
                    devices.append(name)
            # Формат BtbN: нет заголовка, но устройства помечены "(audio)"
            elif "(audio)" in low:
                if name not in devices:
                    devices.append(name)

        return devices

    def _is_windows_system_device(self, name):
        """Определяет, является ли устройство системным (loopback/стерео микшер)."""
        low = (name or "").lower()
        markers = (
            "virtual-audio-capturer",
            "stereo mix",
            "what u hear",
            "wave out",
            "mixage st",
            "loopback",
        )
        return any(m in low for m in markers)

    def _pick_windows_system_device(self, devices):
        """Выбирает подходящее системное устройство из списка."""
        for name in devices:
            if name.lower() == "virtual-audio-capturer":
                return name
        for name in devices:
            if self._is_windows_system_device(name):
                return name
        return None

    def _pick_windows_mic_device(self, devices, system_device):
        """Выбирает микрофон, отличный от заданного системного устройства."""
        for name in devices:
            if system_device and name == system_device:
                continue
            if not self._is_windows_system_device(name):
                return name
        for name in devices:
            if system_device and name == system_device:
                continue
            return name
        return None

    def _start_windows_ffmpeg(self, command_candidates):
        """Пробует запустить кандидаты команд ffmpeg и возвращает первый живой процесс."""
        for cmd in command_candidates:
            try:
                proc = self._popen(cmd)
            except Exception:
                continue

            time.sleep(0.35)
            if proc.poll() is None:
                return proc

            try:
                proc.terminate()
            except Exception:
                pass

        return None

    def _windows_input(self, device_name):
        """Собирает аргументы входа dshow для указанного устройства."""
        return ["-f", "dshow", "-i", f"audio={device_name}"]

    def _build_windows_mix_command(self, first_device, second_device, out_file):
        """Команда ffmpeg для микширования двух устройств в один файл."""
        return [
            self.ffmpeg_bin, "-y",
            *self._windows_input(first_device),
            *self._windows_input(second_device),
            "-filter_complex",
            "[0:a][1:a]amix=inputs=2:normalize=0:duration=longest:dropout_transition=0[aout]",
            "-map", "[aout]",
            "-ac", "2",
            out_file,
        ]

    def _build_windows_single_command(self, device_name, out_file):
        """Команда ffmpeg для записи одного устройства в файл."""
        return [self.ffmpeg_bin, "-y", *self._windows_input(device_name), "-ac", "2", out_file]

    def _build_windows_wasapi_default_command(self, out_file):
        """Команда записи дефолтного WASAPI-устройства в файл."""
        return [self.ffmpeg_bin, "-y", "-f", "wasapi", "-i", "default", "-ac", "2", out_file]

    def _list_windows_wasapi_devices(self):
        """Возвращает списки render и capture устройств WASAPI."""
        try:
            proc = subprocess.run(
                [self.ffmpeg_bin, "-hide_banner", "-list_devices", "true", "-f", "wasapi", "-i", "dummy"],
                capture_output=True, text=True, check=False, timeout=10,
            )
            output = (proc.stdout or "") + "\n" + (proc.stderr or "")
        except Exception:
            return [], []

        render_devices = []
        capture_devices = []
        current_section = None
        for raw_line in output.splitlines():
            line = raw_line.strip()
            low = line.lower()
            if "wasapi" in low and "render devices" in low:
                current_section = "render"
                continue
            if "wasapi" in low and "capture devices" in low:
                current_section = "capture"
                continue

            first = line.find('"')
            if first == -1:
                continue
            second = line.find('"', first + 1)
            if second == -1:
                continue
            name = line[first + 1:second].strip()
            if not name or name.lower() == "default":
                continue

            if current_section == "render":
                render_devices.append(name)
            elif current_section == "capture":
                capture_devices.append(name)

        return render_devices, capture_devices

    def _start_windows_wasapi_loopback(self):
        """Запускает WASAPI loopback через ffmpeg для захвата системного звука."""
        render_devices, _ = self._list_windows_wasapi_devices()
        if not render_devices:
            return False
        device = render_devices[0]
        try:
            proc = self._popen(
                [self.ffmpeg_bin, "-y", "-f", "wasapi", "-loopback", "1", "-i", device,
                 "-ac", "2", self.filename])
            time.sleep(0.3)
            if proc.poll() is not None:
                return False
            self.process_system = proc
            self.process_main = proc
            return True
        except Exception:
            return False

    def _start_windows_wasapi_loopback_python(self):
        """WASAPI loopback через pyaudiowpatch в отдельном thread.

        Работает когда ffmpeg собран без WASAPI. Данные пишутся
        напрямую в self.filename (WAV), без внешнего скрипта.
        """
        try:
            import pyaudiowpatch as pyaudio
        except ImportError:
            return False
        try:
            p = pyaudio.PyAudio()
            dev_info = p.get_default_wasapi_loopback()
            p.terminate()
        except (OSError, IOError):
            try:
                p.terminate()
            except Exception:
                pass
            return False

        self._loopback_stop.clear()

        def _capture():
            """Захватывает WASAPI loopback и пишет WAV напрямую в self.filename."""
            import array
            p = pyaudio.PyAudio()
            try:
                dev = p.get_device_info_by_index(dev_info["index"])
            except Exception:
                p.terminate()
                return
            rate = int(dev["defaultSampleRate"])
            channels = 2
            stream = p.open(
                format=pyaudio.paInt16,
                channels=channels,
                rate=rate,
                input=True,
                input_device_index=dev_info["index"],
                frames_per_buffer=4096,
            )
            wf = wave.open(self.filename, "wb")
            wf.setnchannels(channels)
            wf.setsampwidth(2)
            wf.setframerate(rate)
            try:
                while not self._loopback_stop.is_set():
                    data = stream.read(4096, exception_on_overflow=False)
                    wf.writeframes(data)
            finally:
                stream.stop_stream()
                stream.close()
                wf.close()
                p.terminate()

        self._loopback_thread = threading.Thread(target=_capture, daemon=True)
        self._loopback_thread.start()
        time.sleep(0.5)
        if self._loopback_stop.is_set() or not self._loopback_thread.is_alive():
            return False
        self.process_system = True
        self.process_main = True
        return True

    def _start_windows_mic_capture(self):
        """Запускает запись микрофона в mic_temp через WASAPI или dshow."""
        mic_device = self.default_source
        if not mic_device:
            return False
        try:
            proc = self._popen(
                [self.ffmpeg_bin, "-y", "-f", "wasapi", "-i", mic_device,
                 "-ac", "2", self.mic_temp])
            time.sleep(0.3)
            if proc.poll() is None:
                self.process_mic = proc
                return True
        except Exception:
            pass
        try:
            proc = self._popen(
                [self.ffmpeg_bin, "-y", "-f", "dshow", "-i", f"audio={mic_device}",
                 "-ac", "2", self.mic_temp])
            time.sleep(0.3)
            if proc.poll() is None:
                self.process_mic = proc
                return True
        except Exception:
            pass
        return False

    def _start_windows_all_sources(self):
        """Запускает запись всех источников (система + микрофон) на Windows."""
        # 1. Пробуем WASAPI loopback через ffmpeg (если ffmpeg собран с wasapi)
        if self._start_windows_wasapi_loopback():
            self._start_windows_mic_capture()
            return

        # 2. Пробуем WASAPI loopback через pyaudiowpatch (subprocess)
        if self._start_windows_wasapi_loopback_python():
            self._start_windows_mic_capture()
            return

        # 3. Fallback: dshow-устройства (Line In / Stereo Mix)
        candidates = []
        system_device = self.default_monitor
        mic_device = self.default_source

        if system_device and mic_device and system_device != mic_device:
            candidates.append(self._build_windows_mix_command(system_device, mic_device, self.filename))
        if system_device:
            candidates.append(self._build_windows_single_command(system_device, self.filename))
        if mic_device:
            candidates.append(self._build_windows_single_command(mic_device, self.filename))
        candidates.append(self._build_windows_wasapi_default_command(self.filename))

        self.process_main = self._start_windows_ffmpeg(candidates)

    def _start_windows_mic_only(self):
        """Запускает запись только микрофона на Windows."""
        render_devices, capture_devices = self._list_windows_wasapi_devices()
        mic_names = []
        if self.default_source:
            mic_names.append(self.default_source)
        mic_names.extend(capture_devices)
        for dev in mic_names:
            try:
                proc = self._popen(
                    [self.ffmpeg_bin, "-y", "-f", "wasapi", "-i", dev,
                     "-ac", "2", self.filename])
                time.sleep(0.3)
                if proc.poll() is None:
                    self.process_main = proc
                    return
            except Exception:
                continue

        candidates = []
        mic_device = self.default_source
        if mic_device:
            candidates.append(self._build_windows_single_command(mic_device, self.filename))
        candidates.append(self._build_windows_wasapi_default_command(self.filename))
        self.process_main = self._start_windows_ffmpeg(candidates)

    def _start_windows_system_only(self):
        """Запускает запись только системного звука на Windows."""
        if self._start_windows_wasapi_loopback():
            return

        candidates = []
        system_device = self.default_monitor
        if system_device:
            candidates.append(self._build_windows_single_command(system_device, self.filename))
        candidates.append(self._build_windows_wasapi_default_command(self.filename))
        self.process_main = self._start_windows_ffmpeg(candidates)

    def _detect_default_sink(self):
        """Возвращает имя дефолтного аудио-sink через pactl или pw-dump."""
        try:
            return subprocess.check_output(['pactl', 'get-default-sink'], timeout=5).decode().strip() or None
        except Exception:
            pass
        sink, _ = self._pw_defaults()
        return sink or None

    def _detect_default_source(self):
        """Возвращает имя дефолтного аудио-source через pactl или pw-dump."""
        try:
            return subprocess.check_output(['pactl', 'get-default-source'], timeout=5).decode().strip() or None
        except Exception:
            pass
        _, source = self._pw_defaults()
        return source or None

    def get_available_sources(self):
        """Возвращает список доступных источников."""
        return self.sources

    def _stop_proc(self, proc):
        """Останавливает процесс, постепенно переходя от SIGINT к terminate/kill."""
        if proc is None:
            return
        try:
            proc.send_signal(signal.SIGINT)
            proc.wait(timeout=3)
        except Exception:
            try:
                proc.terminate()
                proc.wait(timeout=2)
            except Exception:
                try:
                    proc.kill()
                except Exception:
                    pass

    def _cleanup_links(self):
        """Удаляет созданные pw-link соединения и очищает список."""
        for a, b in self._pw_links:
            try:
                subprocess.run(["pw-link", "-d", str(a), str(b)],
                    check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
            except Exception:
                pass
        self._pw_links = []

    def _prepare_files(self):
        """Удаляет старые файлы записи перед новым запуском."""
        for p in [self.filename, self.mic_temp]:
            if os.path.exists(p):
                try:
                    os.remove(p)
                except Exception:
                    pass

    def _verify_wav(self, path):
        """Проверяет, что файл является валидным WAV достаточного размера."""
        if not os.path.exists(path) or os.path.getsize(path) < 2000:
            return False
        try:
            with open(path, "rb") as f:
                header = f.read(12)
                if header[:4] != b"RIFF" or header[8:12] != b"WAVE":
                    return False
            return True
        except Exception:
            return False

    def _merge_mic(self):
        """Микширует микрофон в основной файл через amix, восстанавливая бэкап при сбое."""
        if not self._verify_wav(self.mic_temp):
            return
        import shutil
        bak = self.filename + ".bak"
        if os.path.exists(bak):
            os.remove(bak)
        if os.path.exists(self.filename):
            shutil.move(self.filename, bak)

        merge_proc = self._popen(
            [self.ffmpeg_bin, "-y",
             "-i", bak, "-i", self.mic_temp,
             "-filter_complex",
             "[0:a][1:a]amix=inputs=2:normalize=0:duration=longest[a];[a]volume=3dB[aout]",
             "-map", "[aout]", "-ac", "2", self.filename])
        try:
            merge_proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            merge_proc.kill()
            merge_proc.wait(timeout=2)
            if os.path.exists(bak):
                shutil.move(bak, self.filename)
            return

        if merge_proc.returncode != 0:
            if os.path.exists(bak):
                shutil.move(bak, self.filename)
            return

        if self._verify_wav(self.filename):
            if os.path.exists(bak):
                os.remove(bak)
        else:
            if os.path.exists(bak):
                shutil.move(bak, self.filename)

    def _start_pw_tap(self):
        """Записывает системный звук через pw-record, связав мониторные порты sink."""
        self._cleanup_links()
        sink_name = self.default_sink
        if not sink_name:
            return False

        self._pw_record_proc = self._popen(
            ["pw-record", "--target", "0", "--rate", "48000", "--channels", "2", "--format", "s16", self.filename])

        # ждём появления узла pw-record
        deadline = time.time() + 2.0
        dump = None
        while time.time() < deadline:
            time.sleep(0.05)
            try:
                test = json.loads(subprocess.check_output(["pw-dump"], stderr=subprocess.DEVNULL))
                for obj in test:
                    if "Node" not in obj.get("type", ""):
                        continue
                    app = obj.get("info", {}).get("props", {}).get("application.name", "")
                    if "pw-record" in app:
                        dump = test
                        break
                if dump:
                    break
            except Exception:
                continue
        if dump is None:
            self._stop_proc(self._pw_record_proc)
            return False

        rec_node_id = None
        rec_input_ports = []
        for obj in dump:
            if "Node" not in obj.get("type", ""):
                continue
            app = obj.get("info", {}).get("props", {}).get("application.name", "")
            if "pw-record" not in app:
                continue
            rec_node_id = obj.get("id")
            for pt in obj.get("info", {}).get("ports", []):
                if pt.get("direction") == "input":
                    rec_input_ports.append(pt.get("id"))

        if not rec_input_ports:
            for obj in dump:
                if "Port" not in obj.get("type", ""):
                    continue
                info = obj.get("info", {})
                pprops = info.get("props", {})
                if pprops.get("node.id") == rec_node_id and info.get("direction") == "input":
                    rec_input_ports.append(obj.get("id"))

        # Отводим мониторные порты ВСЕХ устройств вывода: пишем системный звук
        # с каждой пары колонок параллельно обычному воспроизведению, ничего
        # не забирая у приложений. Микширование выполняет сам PipeWire.
        all_sink_ids = set()
        for obj in dump:
            if "Node" not in obj.get("type", ""):
                continue
            props = obj.get("info", {}).get("props", {}) or {}
            if props.get("media.class") == "Audio/Sink":
                all_sink_ids.add(obj.get("id"))
        if not all_sink_ids and sink_name:
            for obj in dump:
                if "Node" not in obj.get("type", ""):
                    continue
                nm = obj.get("info", {}).get("props", {}).get("node.name", "")
                if nm == sink_name:
                    all_sink_ids.add(obj.get("id"))
                    break

        sink_monitor_ports = []
        for obj in dump:
            if "Port" not in obj.get("type", ""):
                continue
            info = obj.get("info", {})
            pprops = info.get("props", {})
            if pprops.get("node.id") in all_sink_ids and info.get("direction") == "output" and pprops.get("port.monitor"):
                sink_monitor_ports.append(obj.get("id"))

        if not sink_monitor_ports or not rec_input_ports:
            self._stop_proc(self._pw_record_proc)
            return False

        for i, mp in enumerate(sink_monitor_ports):
            rp = rec_input_ports[i % len(rec_input_ports)]
            r = subprocess.run(["pw-link", str(mp), str(rp)],
                capture_output=True, text=True, timeout=5)
            if r.returncode == 0:
                self._pw_links.append((mp, rp))
            else:
                self._cleanup_links()
                self._stop_proc(self._pw_record_proc)
                return False

        self.audio_capture_time = time.time()
        self.audio_silence_duration = self.audio_capture_time - (self.audio_start_time or self.audio_capture_time)
        return True

    def _start_mic_capture(self):
        """Запускает запись микрофонов в mic_temp через ffmpeg (pulse)."""
        mics = [s for s in self.sources if ".monitor" not in s]
        ordered = []
        if self.default_source and self.default_source in mics:
            ordered.append(self.default_source)
        for m in mics:
            if m not in ordered:
                ordered.append(m)
        if not ordered:
            return False
        cmd = [self.ffmpeg_bin, "-y"]
        for mic_src in ordered:
            cmd += ["-f", "pulse", "-i", mic_src]
        if len(ordered) == 1:
            cmd += ["-ac", "2", self.mic_temp]
        else:
            labels = "".join(f"[{i}:a]" for i in range(len(ordered)))
            cmd += ["-filter_complex",
                f"{labels}amix=inputs={len(ordered)}:normalize=0:duration=longest[aout]",
                "-map", "[aout]", "-ac", "2", self.mic_temp]
        try:
            self.process_mic = self._popen(cmd)
            return True
        except Exception:
            return False

    def start_recording(self, source_indices=None):
        """Начинает запись выбранных источников; если список не задан — всех."""
        if self.process_main is not None:
            return
        if self.is_windows:
            self.audio_start_time = time.time()
            self._prepare_files()
            self._refresh_sources()
            self._start_windows_all_sources()
            return
        if self.is_macos:
            self.audio_start_time = time.time()
            self._start_macos_all_sources()
            return
        self._prepare_files()
        self._refresh_sources()
        if source_indices is None:
            source_indices = list(range(len(self.sources)))
        selected = [self.sources[i] for i in source_indices if i < len(self.sources)]
        if not selected:
            selected = [self.sources[0]] if self.sources else ["default"]
        monitors = [s for s in selected if ".monitor" in s]

        if monitors and self._start_pw_tap():
            self._start_mic_capture()
            self.process_system = self._pw_record_proc
            self.process_main = self.process_system
            return

        cmd = [self.ffmpeg_bin, "-y"]
        for src in selected:
            cmd += ["-f", "pulse", "-i", src]
        if len(selected) == 1:
            cmd += ["-ac", "2", self.filename]
        else:
            labels = "".join(f"[{i}:a]" for i in range(len(selected)))
            cmd += ["-filter_complex",
                f"{labels}amix=inputs={len(selected)}:normalize=0:duration=longest:dropout_transition=0[aout];[aout]volume=12dB[aoutvol]",
                "-map", "[aoutvol]", "-ac", "2", self.filename]
        try:
            self.process_main = self._popen(cmd)
        except Exception:
            self.process_main = None

    def start_mic_only(self):
        """Начинает запись только микрофона."""
        if self.process_main is not None:
            return
        if self.is_windows:
            self._prepare_files()
            self._refresh_sources()
            self._start_windows_mic_only()
            return
        if self.is_macos:
            self._start_macos_mic_only()
            return
        self._prepare_files()
        self._refresh_sources()
        mics = [s for s in self.sources if ".monitor" not in s]
        mic = mics[0] if mics else self.default_source or "default"
        cmd = [self.ffmpeg_bin, "-y", "-f", "pulse", "-i", mic, "-ac", "2", self.filename]
        try:
            self.process_main = self._popen(cmd)
        except Exception:
            self.process_main = None

    def start_system_only(self):
        """Начинает запись только системного звука."""
        if self.process_main is not None:
            return
        if self.is_windows:
            self._prepare_files()
            self._refresh_sources()
            self._start_windows_system_only()
            return
        if self.is_macos:
            self._start_macos_system_only()
            return
        self._prepare_files()
        self._refresh_sources()
        if self.default_sink and self._start_pw_tap():
            self.process_system = self._pw_record_proc
            self.process_main = self.process_system
            return
        monitors = [s for s in self.sources if ".monitor" in s]
        if monitors:
            cmd = [self.ffmpeg_bin, "-y", "-f", "pulse", "-i", monitors[0], "-ac", "2", self.filename]
            try:
                self.process_main = self._popen(cmd)
            except Exception:
                self.process_main = None

    def start_all_sources(self):
        """Начинает запись всех источников (микрофон и системный звук)."""
        if self.process_main is not None:
            return
        self.audio_start_time = time.time()
        if self.is_windows:
            self._prepare_files()
            self._refresh_sources()
            self._start_windows_all_sources()
            self.audio_capture_time = time.time() if self.process_main is not None else None
            if self.audio_capture_time is not None:
                self.audio_silence_duration = self.audio_capture_time - self.audio_start_time
            return
        if self.is_macos:
            self._start_macos_all_sources()
            self.audio_capture_time = time.time() if self.process_main is not None else None
            if self.audio_capture_time is not None:
                self.audio_silence_duration = self.audio_capture_time - self.audio_start_time
            return
        self._prepare_files()
        self._refresh_sources()
        monitors = [s for s in self.sources if ".monitor" in s]

        if monitors and self._start_pw_tap():
            self._start_mic_capture()
            self.process_system = self._pw_record_proc
            self.process_main = self.process_system
            return

        mics = [s for s in self.sources if ".monitor" not in s]
        inputs = []
        if self.default_monitor:
            inputs.append(self.default_monitor)
        for m in monitors:
            if m not in inputs:
                inputs.append(m)
        if self.default_source:
            inputs.append(self.default_source)
        for m in mics:
            if m not in inputs:
                inputs.append(m)
        if not inputs:
            inputs = [self.default_source or "default"]
        cmd = [self.ffmpeg_bin, "-y"]
        for src in inputs:
            cmd += ["-f", "pulse", "-i", src]
        if len(inputs) == 1:
            cmd += ["-ac", "2", self.filename]
        else:
            labels = "".join(f"[{i}:a]" for i in range(len(inputs)))
            cmd += ["-filter_complex",
                f"{labels}amix=inputs={len(inputs)}:normalize=0:duration=longest:dropout_transition=0[aout];[aout]volume=12dB[aoutvol]",
                "-map", "[aoutvol]", "-ac", "2", self.filename]
        try:
            self.process_main = self._popen(cmd)
        except Exception:
            self.process_main = None

    def _start_pipewire_recording(self):
        """Обёртка: запускает запись через PulseAudio-реализацию."""
        self._start_pulseaudio_recording()

    def _start_pulseaudio_recording(self):
        """Обёртка: запускает запись всех источников через start_all_sources."""
        self.start_all_sources()

    def start_recording_with_chunks(self, chunk_callback=None, chunk_seconds=15):
        """Запускает запись и периодически копирует куски файла через callback."""
        self.start_all_sources()
        import threading
        def monitor_chunks():
            """Периодически копирует растущий файл в сегменты и вызывает callback."""
            import time as tm
            chunk_num = 0
            while self.process_main and (
                    self.process_main is True
                    or getattr(self.process_main, "poll", lambda: None)() is None):
                tm.sleep(chunk_seconds)
                if os.path.exists(self.filename) and os.path.getsize(self.filename) > 1000:
                    chunk_file = os.path.join(self.segment_dir, f"chunk_{chunk_num}.wav")
                    try:
                        import shutil
                        shutil.copy(self.filename, chunk_file)
                        if chunk_callback:
                            chunk_callback(chunk_file, chunk_num)
                        chunk_num += 1
                    except Exception:
                        pass
        self.chunk_thread = threading.Thread(target=monitor_chunks, daemon=True)
        self.chunk_thread.start()

    def stop_recording(self):
        """Останавливает все процессы записи и микширует микрофон."""
        # Порядок важен: сначала корректно останавливаем pw-record, чтобы
        # PipeWire сам освободил ссылки на мониторы sink'ов (иначе срыв ссылок
        # до остановки пишущего процесса может «подвесить» аудиографик и
        # обрывать звук на USB-гарнитуре — приходится переподключать её).
        self._stop_proc(self._pw_record_proc)
        time.sleep(0.4)
        self._cleanup_links()
        self._stop_proc(self.process_mic)
        if self._loopback_thread and self._loopback_thread.is_alive():
            self._loopback_stop.set()
            self._loopback_thread.join(timeout=3)
        elif self.process_main is not True:
            self._stop_proc(self.process_main)
        self._pw_record_proc = None
        self.process_mic = None
        self.process_main = None
        self.process_system = None
        self._loopback_thread = None
        self._merge_mic()
