"""Копирует заглушку nv_one_logger...pytorch_lightning в site-packages.

NeMo 2.x импортирует этот модуль при загрузке, но пакет не опубликован
на PyPI. Без заглушки PyInstaller при анализе не может импортировать
ветки nemo и теряет субмодули (в том числе
nemo.collections.asr.modules.sortformer_modules).

Вызывается скриптами сборки ДО pyinstaller. Идемпотентен.
"""

import os
import shutil

import nemo

_STUB_DIR = os.path.dirname(os.path.abspath(__file__))
_STUB_SRC = os.path.join(
    _STUB_DIR, "nv_one_logger", "training_telemetry", "integration",
    "pytorch_lightning.py",
)

_SITE = os.path.dirname(os.path.dirname(nemo.__file__))
_DST_DIR = os.path.join(
    _SITE, "nv_one_logger", "training_telemetry", "integration",
)


def main() -> None:
    """Копирует файл заглушки телеметрии в site-packages рядом с пакетом nemo, если он ещё не установлен."""
    if not os.path.isdir(os.path.join(_SITE, "nv_one_logger")):
        print("[nemo-stub] nv_one_logger not installed, skipping")
        return
    os.makedirs(_DST_DIR, exist_ok=True)
    init_path = os.path.join(_DST_DIR, "__init__.py")
    if not os.path.exists(init_path):
        with open(init_path, "w"):
            pass
    dst_path = os.path.join(_DST_DIR, "pytorch_lightning.py")
    if not os.path.exists(dst_path):
        shutil.copy(_STUB_SRC, dst_path)
        print("[nemo-stub] installed:", dst_path)
    else:
        print("[nemo-stub] already present")


if __name__ == "__main__":
    main()
