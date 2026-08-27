"""Симуляция свежей установки AppImage на дев-окружении (без сборки).

Воспроизводит условия, при которых пользователь получил молчаливые ошибки:
  * sys.frozen = True, бинарник в read-only каталоге (/tmp/.mount_*),
  * XDG_DATA_HOME указывает на несуществующий вложенный путь,
  * никакой каталог данных не создан заранее.

Запуск: .venv/bin/python scripts/test_fresh_install.py
"""
import json
import os
import queue
import shutil
import stat
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

CHILD_SOURCE = r'''
import json
import os
import queue
import shutil
import stat
import sys
import tempfile

sys.frozen = True
sys.path.insert(0, os.path.join(os.getcwd(), "src"))

failures = []


def check(name, condition, detail=""):
    status = "OK" if condition else "FAIL"
    print(f"[{status}] {name}" + (f" -> {detail}" if detail and not condition else ""))
    if not condition:
        failures.append(name)


# ── Окружение «свежий AppImage» ──────────────────────────────────────────
mount = tempfile.mkdtemp(prefix="mount_ro_")
os.makedirs(os.path.join(mount, "usr", "bin"))
open(os.path.join(mount, "usr", "bin", "ScreenRecorder"), "w").close()
# Заводские дефолты поставляются вместе с приложением (в реальном AppImage —
# в _internal/, здесь рядом с бинарником для fallback-цепочки)
import shutil

shutil.copy(
    os.path.join(os.getcwd(), "src", "default_settings.json"),
    os.path.join(mount, "usr", "bin", "default_settings.json"),
)
for dirpath, dirnames, filenames in os.walk(mount):
    os.chmod(dirpath, stat.S_IRUSR | stat.S_IXUSR)
    for fn in filenames:
        os.chmod(os.path.join(dirpath, fn), stat.S_IRUSR)
assert not os.access(os.path.join(mount, "usr", "bin"), os.W_OK), "маунт должен быть read-only"
sys.executable = os.path.join(mount, "usr", "bin", "ScreenRecorder")

xdg_root = sys.argv[1]
os.environ["XDG_DATA_HOME"] = xdg_root  # вложенный путь, ещё не существует
custom_models_parent = sys.argv[2]

import app_paths

# T1: ensure_writable_base_dir создаёт каталог с нуля
base = app_paths.ensure_writable_base_dir()
check("T1 ensure создаёт базовый каталог",
      base == os.path.join(xdg_root, "ScreenRecorder") and os.path.isdir(base), base)

# T2: save_settings сразу после старта, БЕЗ скачивания моделей (гонка из бага)
import gui_window as gw
from settings_manager import config_file
gw.save_settings({"transcription_engine": "whisper", "expected_speakers": "2"})
saved = gw.load_settings()
check("T2 настройки сохраняются в свежий каталог (legacy 'whisper' мигрирует в 'whisperx')",
      saved.get("transcription_engine") == "whisperx" and saved.get("expected_speakers") == "2"
      and os.path.isfile(config_file()), str(saved))

# T3: hf_token пишется в файл (keyring отключаем для детерминизма)
import model_manager as mm
mm._save_keyring = lambda token, **kw: False
mm._try_keyring = lambda **kw: None
mm.save_hf_token("test-token-123")
token = mm.load_hf_token()
check("T3 токен сохраняется и читается",
      token == "test-token-123" and os.path.isfile(mm.HF_TOKEN_FILE), repr(token))

# T4: воркер скачивания применяет настройку models_dir (<setting>/models)
import background_tasks as bg

captured = {}


def fake_snapshot(**kwargs):
    captured.clear()
    captured.update(kwargs)
    os.makedirs(kwargs["local_dir"], exist_ok=True)
    for fn in ("diar_sortformer_4spk-v1.nemo",):
        open(os.path.join(kwargs["local_dir"], fn), "w").close()


mm.login = lambda **kw: True
mm.snapshot_download = fake_snapshot

events = queue.Queue()
bg.download_model_worker("nemo", "fake-token", events, custom_models_parent)
seen = {}
while True:
    try:
        event, payload = events.get_nowait()
    except queue.Empty:
        break
    seen.setdefault(event, []).append(payload)

expected_dir = os.path.join(custom_models_parent, "models", "diar_sortformer_4spk-v1")
print("worker events:", {k: v for k, v in seen.items()})
snap = captured  # fake_snapshot кладёт kwargs плоско (local_dir, allow_patterns, ...)
check("T4a воркер качает в <models_dir>/models/...", snap.get("local_dir") == expected_dir,
      snap.get("local_dir", "<нет вызова>"))
patterns = snap.get("allow_patterns")
required_patterns = next(s["download_patterns"] for s in mm.MODEL_SPECS.values() if s["local_dir"] == expected_dir)
check("T4b передаются allow_patterns (без лишних весов)",
      patterns == required_patterns and "model.safetensors" not in (patterns or []),
      str(patterns))
check("T4c воркер отчитается успехом", seen.get("done") == [{"ok": True}], str(seen))
check("T4d модель найдена по новому пути", mm.is_model_downloaded("nemo"))

# T5: единое разрешение пути для обоих менеджеров
custom2 = custom_models_parent + "_2"
bg._apply_models_dir(custom2)
from audio_transcriber_service import NEMO_MODEL_DIRNAME
check("T5 postprocess использует тот же <models>/models путь",
      mm.get_model_dir() == os.path.join(custom2, "models")
      and NEMO_MODEL_DIRNAME == "diar_sortformer_4spk-v1",
      f"{mm.get_model_dir()} / {NEMO_MODEL_DIRNAME}")

shutil.rmtree(mount, ignore_errors=True)

print("RESULT:", "PASS" if not failures else "FAIL:" + ",".join(failures))
sys.exit(0 if not failures else 1)
'''


def main():
    tmp = tempfile.mkdtemp(prefix="fresh_install_test_")
    xdg_fresh = os.path.join(tmp, "xdg", "nested", "does", "not", "exist")
    custom_models = os.path.join(tmp, "my-models")
    os.makedirs(custom_models)

    proc = subprocess.run(
        [sys.executable, "-c", CHILD_SOURCE, xdg_fresh, custom_models],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=600,
    )
    print(proc.stdout)
    if proc.returncode != 0:
        print(proc.stderr[-3000:])
        print("RESULT: FAIL (child crashed)")
        return 1

    # Проверки в родителе: артефакты симуляции лежат где ожидали
    settings_file = os.path.join(xdg_fresh, "ScreenRecorder", "settings.json")
    ok_settings = os.path.isfile(settings_file)
    print(f"[{'OK' if ok_settings else 'FAIL'}] P1 settings.json появился в XDG: {settings_file}")
    if ok_settings:
        data = json.load(open(settings_file, encoding="utf-8"))
        print(f"       содержимое: {data}")

    downloaded = os.path.join(custom_models, "models", "diar_sortformer_4spk-v1")
    ok_custom = os.path.isfile(os.path.join(downloaded, "diar_sortformer_4spk-v1.nemo"))
    print(f"[{'OK' if ok_custom else 'FAIL'}] P2 файлы модели легли в пользовательский models_dir: {downloaded}")

    shutil.rmtree(tmp, ignore_errors=True)

    passed = proc.returncode == 0 and ok_settings and ok_custom
    print("RESULT:", "ALL TESTS PASSED" if passed else "FAILED")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
