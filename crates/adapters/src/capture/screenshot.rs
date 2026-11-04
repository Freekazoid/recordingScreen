//! Снимок экрана для выбора области.

use ashpd::desktop::screenshot::Screenshot;
use std::path::{Path, PathBuf};

/// Делает снимок экрана: портал на Wayland, ffmpeg на X11.
pub fn take_screenshot(output: &Path) -> Result<PathBuf, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        take_screenshot_wayland(output)
    } else {
        crate::capture::x11::screenshot(output)?;
        Ok(output.to_path_buf())
    }
}

/// Снимок через xdg-desktop-portal Screenshot.
fn take_screenshot_wayland(output: &Path) -> Result<PathBuf, String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    let uri = runtime.block_on(async {
        let request = Screenshot::request()
            .interactive(false)
            .modal(false)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let response = request.response().map_err(|error| error.to_string())?;
        Ok::<String, String>(response.uri().as_str().to_string())
    })?;
    let url = url::Url::parse(&uri).map_err(|error| format!("URI скриншота: {error}"))?;
    let source = url
        .to_file_path()
        .map_err(|_| format!("портал вернул не файл: {uri}"))?;
    std::fs::copy(&source, output)
        .map_err(|error| format!("копирование скриншота {}: {error}", source.display()))?;
    Ok(output.to_path_buf())
}
