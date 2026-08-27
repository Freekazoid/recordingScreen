from __future__ import annotations

import os
import sys
from datetime import datetime


def _log_dir() -> str:
    """Prefer a writable logs/ inside app data; else cwd/logs."""
    candidates: list[str] = []
    if getattr(sys, "frozen", False):
        try:
            from app_paths import get_writable_base_dir

            candidates.append(
                os.path.join(get_writable_base_dir(), "logs")
            )
        except Exception:
            pass
        base = os.path.dirname(os.path.abspath(sys.executable))
        candidates.append(os.path.join(base, "logs"))
    candidates.append("logs")
    for candidate in candidates:
        try:
            os.makedirs(candidate, exist_ok=True)
            probe = os.path.join(candidate, ".write_probe")
            with open(probe, "w", encoding="utf-8") as fh:
                fh.write("")
            os.unlink(probe)
            return candidate
        except OSError:
            continue
    return "logs"


def write_error_report(category: str, message: str, *, extra: str | None = None) -> str:
    log_dir = _log_dir()
    os.makedirs(log_dir, exist_ok=True)
    filename = f"{category.lower()}.log"
    path = os.path.join(log_dir, filename)
    timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    lines = [f"[{timestamp}] {line}" for line in str(message).splitlines() if line.strip()]
    if not lines:
        lines = [f"[{timestamp}] {message}"]
    if extra:
        lines.extend(f"[{timestamp}] {line}" for line in str(extra).splitlines() if line.strip())
    with open(path, "a", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    return path


def clear_error_report(category: str) -> str:
    log_dir = _log_dir()
    os.makedirs(log_dir, exist_ok=True)
    filename = f"{category.lower()}.log"
    path = os.path.join(log_dir, filename)
    with open(path, "w", encoding="utf-8"):
        pass
    return path


__all__ = ["write_error_report", "clear_error_report"]
