"""Helpers for interacting with xdg-desktop-portal ScreenCast on Wayland."""

from __future__ import annotations

import os
import uuid
from collections.abc import Callable
from typing import Any

_GI_AVAILABLE = False
_GI_ERROR_MESSAGE = ""

try:
    import gi

    gi.require_version("Gio", "2.0")
    gi.require_version("GLib", "2.0")
    from gi.repository import Gio, GLib

    _GI_AVAILABLE = True
except Exception as exc:  # pragma: no cover - availability depends on system packages
    Gio = GLib = None
    _GI_ERROR_MESSAGE = str(exc)


PORTAL_BUS_NAME = "org.freedesktop.portal.Desktop"
PORTAL_OBJECT_PATH = "/org/freedesktop/portal/desktop"
PORTAL_INTERFACE = "org.freedesktop.portal.ScreenCast"
REQUEST_INTERFACE = "org.freedesktop.portal.Request"
SESSION_INTERFACE = "org.freedesktop.portal.Session"

MONITOR_SOURCE_TYPE = 1
WINDOW_SOURCE_TYPE = 2


class WaylandPortalError(RuntimeError):
    """Raised when ScreenCast portal interaction fails."""


def is_wayland_session() -> bool:
    return os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland"


def portal_dependencies_available() -> bool:
    return _GI_AVAILABLE


def portal_dependency_error() -> str:
    return _GI_ERROR_MESSAGE or "gi (PyGObject) module not available"


def _variant_dict(items: dict[str, GLib.Variant]) -> dict[str, GLib.Variant]:
    return items


def _deep_unpack(value: Any) -> Any:
    if hasattr(value, "unpack"):
        try:
            value = value.unpack()
        except Exception:
            pass
    if isinstance(value, dict):
        return {key: _deep_unpack(val) for key, val in value.items()}
    if isinstance(value, (list, tuple)):
        return [_deep_unpack(val) for val in value]
    return value


def _token(prefix: str) -> str:
    safe_prefix = "".join(ch if ch.isalnum() else "_" for ch in prefix)
    return f"{safe_prefix}_{uuid.uuid4().hex}"


def _decide_cursor_mode(
    requested: int,
    available_mask: int | None,
    portal_version: int | None,
    log_callback: Callable | None = None,
) -> int | None:
    if portal_version is not None and portal_version < 2:
        if log_callback:
            log_callback(
                f"Портал ScreenCast (версия {portal_version}) не поддерживает выбор режима курсора"
            )
        return None

    if available_mask is None or available_mask <= 0:
        if log_callback:
            log_callback(
                "Портал не сообщил доступные режимы курсора. Параметр cursor_mode не будет передан"
            )
        return None

    if requested & available_mask:
        return requested

    for mode in (2, 1, 4):
        if mode & available_mask:
            if log_callback:
                log_callback(
                    f"Портал не поддерживает cursor_mode={requested}, используется cursor_mode={mode}"
                )
            return mode

    if log_callback:
        log_callback(
            f"Доступные режимы курсора ({available_mask}) не содержат cursor_mode={requested}. Параметр cursor_mode не будет передан"
        )
    return None


class WaylandPortalSession:
    """High-level helper around org.freedesktop.portal.ScreenCast."""

    def __init__(
        self,
        source_types: int = WINDOW_SOURCE_TYPE,
        multiple: bool = False,
        cursor_mode: int = 2,
        persist_mode: int = 0,
        logger: Any | None = None,
    ):
        if not portal_dependencies_available():
            raise WaylandPortalError(
                "PyGObject (Gio/GLib) недоступен. Установите python3-gi и зависимости GStreamer."
            )

        self.source_types = source_types
        self.multiple = multiple
        self.cursor_mode = cursor_mode
        self.persist_mode = persist_mode
        self.logger = logger
        self._bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        self._portal = Gio.DBusProxy.new_sync(
            self._bus,
            Gio.DBusProxyFlags.NONE,
            None,
            PORTAL_BUS_NAME,
            PORTAL_OBJECT_PATH,
            PORTAL_INTERFACE,
            None,
        )
        self._session_handle: str | None = None
        self._session_proxy: Gio.DBusProxy | None = None

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------
    def open_stream(
        self, parent_window: str = ""
    ) -> dict[str, Any] | None:
        """Request a PipeWire stream via portal dialogs."""

        self._create_session()
        try:
            self._select_sources()
            start_result = self._start_session(parent_window)
        except Exception:
            self.close()
            raise

        if start_result is None:
            self.close()
            return None

        pipewire_fd = self._open_pipewire_remote()
        streams = start_result.get("streams", [])
        restore_token = start_result.get("restore_token")

        return {
            "session_handle": self._session_handle,
            "streams": streams,
            "pipewire_fd": pipewire_fd,
            "restore_token": restore_token,
        }

    def close(self):
        if not portal_dependencies_available():
            return
        if self._session_proxy is None:
            return
        try:
            self._session_proxy.call_sync(
                "Close",
                None,
                Gio.DBusCallFlags.NO_AUTO_START,
                -1,
                None,
            )
        except Exception:
            pass
        finally:
            self._session_proxy = None
            self._session_handle = None

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------
    def _log(self, message: str):
        import datetime
        import os
        timestamp = datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]
        # Пишем только в перезаписываемый каталог: в AppImage каталог бандла
        # read-only, а падение логирования не должно убивать воркер записи.
        try:
            from app_paths import get_writable_base_dir
            log_dir = os.path.join(get_writable_base_dir(), "logs")
            os.makedirs(log_dir, exist_ok=True)
            log_file = os.path.join(log_dir, "wayland.log")
            with open(log_file, "a", encoding="utf-8") as f:
                f.write(f"[{timestamp}] {message}\n")
        except Exception:
            pass
        if self.logger:
            try:
                self.logger(message)
            except Exception:
                pass

    def _ensure_source_type_available(self) -> bool:
        try:
            variant = self._portal.get_cached_property("AvailableSourceTypes")
        except Exception:
            variant = None

        if variant is None:
            return True

        try:
            available_mask = int(variant.unpack())
        except Exception:
            return True

        return bool(available_mask & self.source_types)

    def _create_session(self):
        handle_token = _token("screenrec")
        session_token = _token("session")
        options = _variant_dict(
            {
                "handle_token": GLib.Variant("s", handle_token),
                "session_handle_token": GLib.Variant("s", session_token),
            }
        )
        result = self._portal.call_sync(
            "CreateSession",
            GLib.Variant("(a{sv})", (options,)),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )
        request_path = self._extract_path_result(result)
        response_code, response = self._wait_request(request_path)
        if response_code != 0:
            raise WaylandPortalError("Пользователь отклонил создание сессии ScreenCast")

        session_handle_variant = response.get("session_handle")
        if session_handle_variant is None:
            raise WaylandPortalError("Портал не вернул дескриптор сессии")

        session_handle = (
            session_handle_variant.unpack()
            if hasattr(session_handle_variant, "unpack")
            else session_handle_variant
        )
        self._session_handle = session_handle
        self._session_proxy = Gio.DBusProxy.new_sync(
            self._bus,
            Gio.DBusProxyFlags.NONE,
            None,
            PORTAL_BUS_NAME,
            session_handle,
            SESSION_INTERFACE,
            None,
        )

        try:
            self._session_proxy.connect("g-signal", self._on_session_signal)
        except Exception:
            pass

    def _select_sources(self):
        if not self._session_handle:
            raise WaylandPortalError("Сессия не создана")

        resolved_cursor_mode = self._resolve_cursor_mode()

        options = {
            "handle_token": GLib.Variant("s", _token("select")),
            "types": GLib.Variant("u", self.source_types),
            "multiple": GLib.Variant("b", self.multiple),
        }
        if resolved_cursor_mode is not None:
            options["cursor_mode"] = GLib.Variant("u", resolved_cursor_mode)
        if self.persist_mode:
            options["persist_mode"] = GLib.Variant("u", self.persist_mode)

        result = self._portal.call_sync(
            "SelectSources",
            GLib.Variant("(oa{sv})", (self._session_handle, _variant_dict(options))),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )

        request_path = self._extract_path_result(result)
        response_code, _ = self._wait_request(request_path)
        if response_code != 0:
            raise WaylandPortalError("Запрос источника отклонён пользователем")

    def _resolve_cursor_mode(self) -> int | None:
        requested = int(self.cursor_mode)
        portal_version = self._get_portal_property("version")
        available_mask = self._get_portal_property("AvailableCursorModes")

        return _decide_cursor_mode(
            requested=requested,
            available_mask=available_mask,
            portal_version=portal_version,
            log_callback=self._log,
        )

    def _start_session(self, parent_window: str) -> dict[str, Any] | None:
        if not self._session_handle:
            raise WaylandPortalError("Сессия не создана")

        options = {
            "handle_token": GLib.Variant("s", _token("start")),
        }

        result = self._portal.call_sync(
            "Start",
            GLib.Variant(
                "(osa{sv})",
                (self._session_handle, parent_window or "", _variant_dict(options)),
            ),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )

        request_path = self._extract_path_result(result)
        response_code, response = self._wait_request(request_path)
        if response_code != 0:
            # 1 = cancelled, 2 = denied
            return None

        parsed: dict[str, Any] = {}
        if "streams" in response:
            parsed["streams"] = self._parse_streams(response["streams"])
        if "restore_token" in response:
            val = response["restore_token"]
            parsed["restore_token"] = val.unpack() if hasattr(val, "unpack") else val
        return parsed

    def _open_pipewire_remote(self) -> int:
        if not self._session_handle:
            raise WaylandPortalError("Сессия не создана")

        result, fd_list = self._portal.call_with_unix_fd_list_sync(
            "OpenPipeWireRemote",
            GLib.Variant("(oa{sv})", (self._session_handle, _variant_dict({}))),
            Gio.DBusCallFlags.NONE,
            -1,
            None,
        )
        fd_index = self._extract_fd_index_result(result)
        try:
            raw_fd = fd_list.get(fd_index)
        except Exception:
            raw_fd = fd_list.get(0)
        try:
            dup_fd = os.dup(raw_fd)
        except Exception:
            try:
                os.close(raw_fd)
            except Exception:
                pass
            raise
        os.close(raw_fd)
        return dup_fd

    @staticmethod
    def _extract_path_result(result_variant) -> str:
        unpacked = result_variant.unpack()
        if isinstance(unpacked, (tuple, list)):
            return str(unpacked[0])
        if isinstance(unpacked, dict):
            for key in ("handle", "request_handle", "request_path", "path"):
                if key in unpacked:
                    return str(unpacked[key])
            if unpacked:
                return str(next(iter(unpacked.values())))
        raise WaylandPortalError(f"Неожиданный формат ответа портала: {unpacked!r}")

    def _get_portal_property(self, property_name: str) -> int | None:
        try:
            props_proxy = Gio.DBusProxy.new_sync(
                self._bus,
                Gio.DBusProxyFlags.NONE,
                None,
                PORTAL_BUS_NAME,
                PORTAL_OBJECT_PATH,
                "org.freedesktop.DBus.Properties",
                None,
            )
            result = props_proxy.call_sync(
                "Get",
                GLib.Variant("(ss)", (PORTAL_INTERFACE, property_name)),
                Gio.DBusCallFlags.NONE,
                -1,
                None,
            )
            value = result.unpack()
            if isinstance(value, (tuple, list)):
                value = value[0]
            return int(value)
        except Exception:
            return None

    @staticmethod
    def _extract_fd_index_result(result_variant) -> int:
        unpacked = result_variant.unpack()
        if isinstance(unpacked, (tuple, list)):
            return int(unpacked[0])
        if isinstance(unpacked, dict):
            for key in ("fd", "fd_index", "index", "h"):
                if key in unpacked:
                    return int(unpacked[key])
            if unpacked:
                return int(next(iter(unpacked.values())))
        return int(unpacked)

    def _wait_request(self, request_path: str, timeout: int = 60) -> tuple[int, dict[str, GLib.Variant]]:
        proxy = Gio.DBusProxy.new_sync(
            self._bus,
            Gio.DBusProxyFlags.NONE,
            None,
            PORTAL_BUS_NAME,
            request_path,
            REQUEST_INTERFACE,
            None,
        )

        loop = GLib.MainLoop()
        response_data: dict[str, GLib.Variant] = {}
        response_code = 1

        def _on_signal(_proxy, _sender, signal_name, parameters):
            nonlocal response_code, response_data
            if signal_name != "Response":
                return
            response_code = parameters.get_child_value(0).get_uint32()
            response_variant = parameters.get_child_value(1)
            response_data = response_variant.unpack()
            loop.quit()

        def _on_timeout():
            self._log("Portal не ответил за {timeout}с, прерываем ожидание")
            loop.quit()
            return False

        timeout_id = GLib.timeout_add_seconds(timeout, _on_timeout)
        handler_id = proxy.connect("g-signal", _on_signal)
        try:
            loop.run()
        finally:
            try:
                GLib.Source.remove(timeout_id)
            except Exception:
                pass
            try:
                proxy.disconnect(handler_id)
            except Exception:
                pass

        return response_code, response_data

    @staticmethod
    def _parse_streams(variant) -> list[dict[str, Any]]:
        streams: list[dict[str, Any]] = []
        try:
            unpacked = variant.unpack() if hasattr(variant, "unpack") else variant
        except Exception:
            return streams

        if unpacked is None:
            return streams

        for item in unpacked:
            try:
                node_id, props = item
            except Exception:
                continue
            stream_info: dict[str, Any] = {"node_id": int(node_id)}
            if isinstance(props, dict):
                for key, value in props.items():
                    stream_info[key] = _deep_unpack(value)
            streams.append(stream_info)
        return streams

    def _on_session_signal(self, _proxy, _sender, signal_name, _params):
        if signal_name == "Closed":
            self._log("Портал завершил сессию ScreenCast")


__all__ = [
    "WaylandPortalSession",
    "WaylandPortalError",
    "is_wayland_session",
    "portal_dependencies_available",
    "portal_dependency_error",
    "WINDOW_SOURCE_TYPE",
    "MONITOR_SOURCE_TYPE",
    "_decide_cursor_mode",
]
