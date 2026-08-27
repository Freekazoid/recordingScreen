"""Asynchronous helpers for xdg-desktop-portal ScreenCast via dbus-next."""

from __future__ import annotations

import asyncio
import inspect
import os
import secrets
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any

_DBUS_AVAILABLE = False
_DBUS_IMPORT_ERROR = ""
_DBUS_SUPPORTS_UNIX_FD = False

try:
    from dbus_next import Message, MessageType, Variant
    from dbus_next.aio import MessageBus
    from dbus_next.aio.proxy_object import ProxyInterface
    from dbus_next.errors import InvalidMemberNameError
    from dbus_next.validators import assert_member_name_valid

    _original_member_validator = assert_member_name_valid

    def _relaxed_member_validator(name):
        try:
            _original_member_validator(name)
        except InvalidMemberNameError:
            pass

    import dbus_next.validators as _validators
    _validators.assert_member_name_valid = _relaxed_member_validator
    import dbus_next.introspection as _intr
    _intr.assert_member_name_valid = _relaxed_member_validator
    import dbus_next.message as _msg
    _msg.assert_member_name_valid = _relaxed_member_validator

    _DBUS_AVAILABLE = True
    try:
        _DBUS_SUPPORTS_UNIX_FD = "negotiate_unix_fd" in inspect.signature(MessageBus.__init__).parameters
    except (TypeError, ValueError):
        _DBUS_SUPPORTS_UNIX_FD = False
except Exception as exc:  # pragma: no cover - depends on system/venv packages
    Message = MessageType = Variant = MessageBus = ProxyInterface = None
    _DBUS_IMPORT_ERROR = str(exc)
    _DBUS_SUPPORTS_UNIX_FD = False

PORTAL_BUS_NAME = "org.freedesktop.portal.Desktop"
PORTAL_OBJECT_PATH = "/org/freedesktop/portal/desktop"
SCREENCAST_INTERFACE = "org.freedesktop.portal.ScreenCast"
REQUEST_INTERFACE = "org.freedesktop.portal.Request"
SESSION_INTERFACE = "org.freedesktop.portal.Session"

# SelectSources shows the compositor picker; allow enough time for the user.
_PORTAL_RESPONSE_TIMEOUT_S = 300.0


class PortalError(RuntimeError):
    pass


def dbus_available() -> bool:
    return _DBUS_AVAILABLE


def dbus_supports_unix_fd() -> bool:
    """True when MessageBus can negotiate Unix FDs (needed for PipeWire portal FD)."""
    return _DBUS_AVAILABLE and _DBUS_SUPPORTS_UNIX_FD


def dbus_dependency_error() -> str:
    if _DBUS_IMPORT_ERROR:
        return _DBUS_IMPORT_ERROR
    if _DBUS_AVAILABLE and not _DBUS_SUPPORTS_UNIX_FD:
        return (
            "dbus-next без negotiate_unix_fd "
            "(нужен dbus-next>=0.2.3 или backend PyGObject)"
        )
    return "dbus-next module not available"


def _unwrap(value: Any) -> Any:
    if isinstance(value, Variant):
        return _unwrap(value.value)
    if isinstance(value, dict):
        return {k: _unwrap(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_unwrap(v) for v in value]
    return value


def _token(prefix: str) -> str:
    safe_prefix = "".join(ch if ch.isalnum() else "_" for ch in prefix)
    return f"{safe_prefix}_{secrets.token_hex(6)}"


def _predict_request_path(unique_name: str, handle_token: str) -> str:
    """Build the Request object path before the portal method returns.

    xdg-desktop-portal documents:
    /org/freedesktop/portal/desktop/request/SENDER/TOKEN
    where SENDER is the unique name without leading ':' and with '.' -> '_'.
    """
    sender = unique_name[1:] if unique_name.startswith(":") else unique_name
    sender = sender.replace(".", "_")
    return f"/org/freedesktop/portal/desktop/request/{sender}/{handle_token}"


@dataclass
class StreamInfo:
    node_id: int
    properties: dict[str, Any]


class ScreenCastSession:
    def __init__(self, bus: MessageBus, interface: ProxyInterface, session_handle: str):
        self._bus = bus
        self._interface = interface
        self.session_handle = session_handle

    async def close(self) -> None:
        await self._interface.bus.call(Message(
            destination=PORTAL_BUS_NAME,
            path=self.session_handle,
            interface=SESSION_INTERFACE,
            member="Close",
        ))


class ScreenCastPortal:
    def __init__(self):
        self._bus: MessageBus | None = None
        self._interface: ProxyInterface | None = None

    async def connect(self) -> None:
        if not dbus_available():
            raise PortalError(f"dbus-next недоступен: {dbus_dependency_error()}")
        if not dbus_supports_unix_fd():
            raise PortalError(
                "dbus-next не поддерживает negotiate_unix_fd "
                "(обновите до >=0.2.3 или используйте PyGObject backend)"
            )
        self._bus = await MessageBus(negotiate_unix_fd=True).connect()
        introspection = await self._bus.introspect(PORTAL_BUS_NAME, PORTAL_OBJECT_PATH)
        proxy = self._bus.get_proxy_object(PORTAL_BUS_NAME, PORTAL_OBJECT_PATH, introspection)
        self._interface = proxy.get_interface(SCREENCAST_INTERFACE)

    async def create_session(self, persist: bool = False) -> ScreenCastSession:
        if not self._interface or not self._bus:
            raise PortalError("Portal not connected")

        session_token = _token("session")
        handle_token = _token("handle")
        options = {
            "session_handle_token": Variant("s", session_token),
            "handle_token": Variant("s", handle_token),
        }
        if persist:
            options["persist_mode"] = Variant("u", persist)

        response = await self._call_with_response(
            handle_token,
            lambda: self._interface.call_create_session(options),
        )
        code = response[0]
        if code != 0:
            raise PortalError(_response_error("CreateSession", code))
        payload = _unwrap(response[1])
        session_handle = payload.get("session_handle")
        if not session_handle:
            raise PortalError("Portal did not return session handle")
        return ScreenCastSession(self._bus, self._interface, session_handle)

    async def select_sources(
        self,
        session: ScreenCastSession,
        types: int,
        multiple: bool = False,
        cursor_mode: int = 2,
    ) -> None:
        if not self._interface:
            raise PortalError("Portal not connected")
        resolved_cursor_mode = await self._resolve_cursor_mode(cursor_mode)
        handle_token = _token("handle")
        response = await self._call_with_response(
            handle_token,
            lambda: self._interface.call_select_sources(
                session.session_handle,
                {
                    "types": Variant("u", types),
                    "multiple": Variant("b", multiple),
                    "cursor_mode": Variant("u", resolved_cursor_mode),
                    "handle_token": Variant("s", handle_token),
                },
            ),
        )
        code = response[0]
        if code != 0:
            raise PortalError(_response_error("SelectSources", code))

    async def start(self, session: ScreenCastSession, parent_window: str = "") -> list[StreamInfo]:
        if not self._interface:
            raise PortalError("Portal not connected")
        handle_token = _token("handle")
        response = await self._call_with_response(
            handle_token,
            lambda: self._interface.call_start(
                session.session_handle,
                parent_window,
                {"handle_token": Variant("s", handle_token)},
            ),
        )
        code = response[0]
        if code != 0:
            raise PortalError(_response_error("Start", code))
        payload = _unwrap(response[1])
        streams: list[StreamInfo] = []
        for entry in payload.get("streams", []):
            if isinstance(entry, (list, tuple)) and len(entry) == 2:
                node_id = int(entry[0])
                props = entry[1] if isinstance(entry[1], dict) else {}
                streams.append(StreamInfo(node_id=node_id, properties=_unwrap(props)))
        return streams

    async def open_pipewire_remote(self, session: ScreenCastSession) -> int:
        if not self._interface:
            raise PortalError("Portal not connected")
        reply = await self._interface.call_open_pipe_wire_remote(
            session.session_handle,
            {},
        )
        raw_fd = _coerce_unix_fd(reply)
        try:
            dup_fd = os.dup(raw_fd)
        except Exception:
            try:
                os.close(raw_fd)
            except Exception:
                pass
            raise
        try:
            os.set_inheritable(dup_fd, True)
        except Exception:
            try:
                os.close(dup_fd)
            except Exception:
                pass
            raise
        try:
            os.close(raw_fd)
        except Exception:
            pass
        return dup_fd

    async def _call_with_response(
        self,
        handle_token: str,
        call: Callable[[], Awaitable[Any]],
        *,
        timeout: float = _PORTAL_RESPONSE_TIMEOUT_S,
    ) -> tuple[int, dict[str, Any]]:
        """Invoke a portal method after subscribing to Request.Response.

        Registering the handler only after the method returns races the portal:
        Response can arrive before we listen, so SelectSources never shows UI.
        """
        if not self._bus or not self._bus.unique_name:
            raise PortalError("Portal not connected")

        request_path = _predict_request_path(self._bus.unique_name, handle_token)
        loop = asyncio.get_running_loop()
        future: asyncio.Future = loop.create_future()

        def handler(message: Message) -> bool:
            if message.message_type != MessageType.SIGNAL:
                return False
            if message.path != request_path:
                return False
            if message.interface != REQUEST_INTERFACE or message.member != "Response":
                return False
            if not future.done():
                future.set_result(message.body)
            return True

        self._bus.add_message_handler(handler)
        try:
            await call()
            try:
                return await asyncio.wait_for(future, timeout=timeout)
            except asyncio.TimeoutError as exc:
                raise PortalError(
                    f"Портал не ответил за {int(timeout)}с (path={request_path})"
                ) from exc
        finally:
            self._bus.remove_message_handler(handler)

    async def _resolve_cursor_mode(self, requested: int) -> int:
        if not self._bus:
            return requested
        try:
            reply = await self._bus.call(
                Message(
                    destination=PORTAL_BUS_NAME,
                    path=PORTAL_OBJECT_PATH,
                    interface="org.freedesktop.DBus.Properties",
                    member="Get",
                    signature="ss",
                    body=[SCREENCAST_INTERFACE, "AvailableCursorModes"],
                )
            )
            if not reply or not reply.body:
                return requested
            value = _unwrap(reply.body[0])
            available_mask = int(value)
        except Exception:
            return requested

        if requested & available_mask:
            return requested
        for mode in (2, 1, 4):
            if mode & available_mask:
                return mode
        return requested


__all__ = [
    "ScreenCastPortal",
    "ScreenCastSession",
    "StreamInfo",
    "PortalError",
    "dbus_available",
    "dbus_dependency_error",
    "dbus_supports_unix_fd",
]


def _coerce_unix_fd(reply: Any) -> int:
    """Normalize OpenPipeWireRemote reply to a raw FD int.

    dbus-next high-level API returns a single DBus 'h' as a plain int, not [fd].
    """
    if reply is None:
        raise PortalError("Portal did not return PipeWire fd")
    if isinstance(reply, bool):
        raise PortalError(f"Portal returned unexpected FD value: {reply!r}")
    if isinstance(reply, int):
        return reply
    if hasattr(reply, "take"):
        return int(reply.take())
    if isinstance(reply, (list, tuple)) and reply:
        return _coerce_unix_fd(reply[0])
    try:
        return int(reply)
    except Exception as exc:
        raise PortalError(f"Portal returned unexpected FD value: {reply!r}") from exc


def _response_error(method: str, code: int) -> str:
    if code == 1:
        return f"{method}: действие отменено пользователем"
    if code == 2:
        return f"{method}: доступ отклонен порталом/композитором"
    return f"{method}: ошибка портала, код={code}"
