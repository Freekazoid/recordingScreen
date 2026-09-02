"""Запись системного звука через WASAPI loopback с pyaudiowpatch.

Использование: python wasapi_loopback_record.py <output.wav> [duration_seconds]

Захватывает системный звук через WASAPI loopback до сигнала SIGINT или
истечения заданной длительности. Пишет PCM s16le stereo WAV с частотой
дискретизации устройства.
"""
import signal
import struct
import sys
import time
import wave

_stop = False


def _on_signal(sig, frame):
    """Устанавливает флаг остановки по сигналу (SIGINT/SIGTERM)."""
    global _stop
    _stop = True


def main():
    if len(sys.argv) < 2:
        sys.exit(1)
    out_path = sys.argv[1]
    max_duration = float(sys.argv[2]) if len(sys.argv) > 2 else 0

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    try:
        import pyaudiowpatch as pyaudio
    except ImportError:
        sys.exit(2)

    p = pyaudio.PyAudio()

    try:
        dev_info = p.get_default_wasapi_loopback()
    except OSError:
        p.terminate()
        sys.exit(4)
    loopback_idx = dev_info["index"]

    dev = p.get_device_info_by_index(loopback_idx)
    rate = int(dev["defaultSampleRate"])
    channels = 2
    fmt = pyaudio.paInt16

    stream = p.open(
        format=fmt,
        channels=channels,
        rate=rate,
        input=True,
        input_device_index=loopback_idx,
        frames_per_buffer=4096,
    )

    wf = wave.open(out_path, "wb")
    wf.setnchannels(channels)
    wf.setsampwidth(2)
    wf.setframerate(rate)

    start = time.monotonic()
    try:
        while not _stop:
            if max_duration > 0 and (time.monotonic() - start) >= max_duration:
                break
            data = stream.read(4096, exception_on_overflow=False)
            wf.writeframes(data)
    finally:
        stream.stop_stream()
        stream.close()
        wf.close()
        p.terminate()


if __name__ == "__main__":
    main()
