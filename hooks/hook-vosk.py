"""PyInstaller hook for vosk: collect libvosk.dll and companion DLLs."""
from PyInstaller.utils.hooks import collect_dynamic_libs, collect_data_files

binaries = collect_dynamic_libs("vosk")
datas = collect_data_files("vosk")
