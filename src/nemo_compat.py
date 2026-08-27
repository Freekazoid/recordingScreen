"""Совместимость NeMo с окружением без пакета nv_one_logger_*_lightning.

NeMo 2.x импортирует телеметрию обучения (nv_one_logger), но пакет
интеграции с PyTorch Lightning не опубликован на PyPI. Мы используем
только инференс, поэтому подменяем модуль заглушкой.
"""

import os

# @torch.jit.script в NeMo требует доступа к исходникам модулей, которых
# нет в заморозке (PyInstaller). PYTORCH_JIT=0 отключает скриптинг:
# декорированные функции работают в обычном режиме eager.
os.environ.setdefault("PYTORCH_JIT", "0")

import sys
import types

_STUB_MODULE = "nv_one_logger.training_telemetry.integration.pytorch_lightning"


def _make_stub():
    integration = types.ModuleType(
        "nv_one_logger.training_telemetry.integration"
    )
    pl_mod = types.ModuleType(_STUB_MODULE)
    try:
        from lightning.pytorch.callbacks import Callback as _Base
    except Exception:
        class _Base:
            pass

    class TimeEventCallback(_Base):
        def __init__(self, *args, **kwargs):
            try:
                super().__init__()
            except Exception:
                pass

    pl_mod.TimeEventCallback = TimeEventCallback
    integration.pytorch_lightning = pl_mod

    telemetry = sys.modules.get("nv_one_logger.training_telemetry")
    if telemetry is not None:
        setattr(telemetry, "integration", integration)
    one_logger = sys.modules.get("nv_one_logger")
    if one_logger is not None:
        training_telemetry = sys.modules.get(
            "nv_one_logger.training_telemetry"
        )
        if training_telemetry is not None:
            setattr(one_logger, "training_telemetry", training_telemetry)

    sys.modules["nv_one_logger.training_telemetry.integration"] = integration
    sys.modules[_STUB_MODULE] = pl_mod


def ensure_nemo_imports():
    """Гарантирует импортируемость nemo.collections.asr."""
    try:
        import nv_one_logger.training_telemetry.integration.pytorch_lightning  # noqa: F401
    except ModuleNotFoundError:
        _make_stub()
