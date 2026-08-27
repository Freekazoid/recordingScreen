#!/usr/bin/env python3
"""Console tool to record Wayland screen/window via xdg-desktop-portal."""

from __future__ import annotations

import argparse
import asyncio
import os
import sys
from pathlib import Path

from ffmpeg_pipewire import RecordingOptions, record_pipewire_stream
from wayland_portal_async import PortalError, ScreenCastPortal

TYPE_MONITOR = 1
TYPE_WINDOW = 2


def _parse_crop(value: str) -> str:
    parts = value.replace("x", ":").replace(",", ":").split(":")
    if len(parts) != 4:
        raise argparse.ArgumentTypeError("crop must be 'x,y,width,height'")
    x, y, w, h = parts
    try:
        xi = int(x)
        yi = int(y)
        wi = int(w)
        hi = int(h)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("crop values must be integers") from exc
    return f"{wi}:{hi}:{xi}:{yi}"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Wayland recorder via xdg-desktop-portal")
    parser.add_argument("output", type=Path, help="Output video file (e.g. capture.mkv)")
    parser.add_argument(
        "--mode",
        choices=["fullscreen", "window", "area"],
        default="fullscreen",
        help="Capture mode",
    )
    parser.add_argument("--fps", type=int, default=30, help="Frames per second")
    parser.add_argument("--duration", type=int, default=10, help="Duration in seconds")
    parser.add_argument("--crop", type=_parse_crop, help="Crop region x,y,width,height (required for area mode)")
    parser.add_argument("--crf", type=int, default=23, help="ffmpeg CRF value")
    parser.add_argument(
        "--preset", default="veryfast", help="ffmpeg x264 preset (ultrafast/veryfast/faster/...)"
    )
    return parser


async def run_capture(args: argparse.Namespace) -> None:
    if os.environ.get("XDG_SESSION_TYPE", "").lower() != "wayland":
        raise RuntimeError("Wayland capture requires XDG_SESSION_TYPE=wayland")

    portal = ScreenCastPortal()
    await portal.connect()
    session = await portal.create_session(persist=2)

    try:
        if args.mode == "window":
            types = TYPE_WINDOW
        else:
            types = TYPE_MONITOR

        await portal.select_sources(session, types, multiple=False)
        streams = await portal.start(session)
        if not streams:
            raise PortalError("Portal did not return any streams")

        stream = streams[0]
        pw_fd = await portal.open_pipewire_remote(session)

        options = RecordingOptions(
            output=str(args.output),
            fps=args.fps,
            duration=args.duration,
            crop=args.crop if args.mode == "area" else None,
            crf=args.crf,
            preset=args.preset,
        )

        print(f"Recording PipeWire node {stream.node_id} -> {args.output}")
        await asyncio.to_thread(record_pipewire_stream, stream.node_id, pw_fd, options)
        print("Recording finished")
    finally:
        await session.close()


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    if args.mode == "area" and not args.crop:
        parser.error("--crop is required for area mode (format: x,y,width,height)")

    try:
        asyncio.run(run_capture(args))
        return 0
    except (PortalError, RuntimeError) as exc:
        parser.error(str(exc))
    except KeyboardInterrupt:
        print("Interrupted", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
