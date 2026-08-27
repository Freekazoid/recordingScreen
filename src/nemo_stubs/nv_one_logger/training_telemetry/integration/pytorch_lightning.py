"""Заглушка невыпубликованного nv_one_logger-плагина (нужен только NeMo).

NeMo 2.x импортирует этот модуль, но пакета нет на PyPI. Скрипты сборки
копируют заглушку в site-packages ДО запуска PyInstaller: иначе анализ
не может импортировать ветки nemo и теряет нужные субмодули (например,
nemo.collections.asr.modules.sortformer_modules).
"""
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
