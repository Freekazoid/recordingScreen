//! Снимок экрана для выбора области.
//!
//! На Wayland portal Screenshot на части систем возвращает
//! «Portal request didn't succeed». Поэтому снимок берётся из того же
//! ScreenCast-потока, который используется для записи: получаем один кадр
//! через GStreamer и сохраняем PNG.

use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Делает снимок экрана: ScreenCast на Wayland, ffmpeg на X11.
pub fn take_screenshot(output: &Path) -> Result<PathBuf, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        take_screenshot_wayland(output)
    } else {
        crate::capture::x11::screenshot(output)?;
        Ok(output.to_path_buf())
    }
}

/// Снимок одного кадра из ScreenCast-потока.
fn take_screenshot_wayland(output: &Path) -> Result<PathBuf, String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    let (proxy, session, fd, node_id) = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), open_stream())
            .await
            .map_err(|_| "таймаут ожидания портала (60 с)".to_string())?
    })?;
    let raw_fd = fd.as_raw_fd();
    if unsafe { libc::fcntl(raw_fd, libc::F_SETFD, 0) } == -1 {
        return Err("не удалось подготовить PipeWire fd".to_string());
    }
    let log_path = output.with_extension("gst.log");
    let log = std::fs::File::create(&log_path)
        .map_err(|error| format!("лог GStreamer {}: {error}", log_path.display()))?;
    let mut child = Command::new("gst-launch-1.0")
        .args([
            "-e",
            "pipewiresrc",
            &format!("fd={raw_fd}"),
            &format!("path={node_id}"),
            "always-copy=true",
            "do-timestamp=true",
            "num-buffers=1",
            "!",
            "videoconvert",
            "!",
            "pngenc",
            "!",
            "filesink",
            &format!("location={}", output.display()),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|error| format!("gst-launch-1.0: {error}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(8) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("таймаут получения кадра (8 с)".to_string());
            }
            Err(error) => return Err(format!("ожидание gst: {error}")),
        }
    };
    drop(fd);
    drop(session);
    drop(proxy);

    if !status.success() {
        let text = std::fs::read_to_string(&log_path).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(8).collect();
        return Err(format!(
            "не удалось получить кадр: {}",
            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
        ));
    }
    if !output.exists() {
        return Err("GStreamer не создал PNG".to_string());
    }
    Ok(output.to_path_buf())
}

/// Открывает ScreenCast-портал и возвращает поток.
async fn open_stream() -> Result<(Screencast, Session<Screencast>, OwnedFd, u32), String> {
    let proxy = Screencast::new()
        .await
        .map_err(|error| format!("Screencast: {error}"))?;
    let session = proxy
        .create_session(Default::default())
        .await
        .map_err(|error| format!("CreateSession: {error}"))?;
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(ashpd::enumflags2::BitFlags::from(SourceType::Monitor))
                .set_multiple(false)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await
        .map_err(|error| format!("SelectSources: {error}"))?;
    let response = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(|error| format!("Start: {error}"))?
        .response()
        .map_err(|error| format!("Start response: {error}"))?;
    let stream = response
        .streams()
        .first()
        .ok_or_else(|| "портал не вернул поток".to_string())?;
    let node_id = stream.pipe_wire_node_id();
    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(|error| format!("OpenPipeWireRemote: {error}"))?;
    Ok((proxy, session, fd, node_id))
}
