//! Встраивает иконку и сведения об издателе в Windows-исполняемый файл.

use std::env;

/// Добавляет ресурс только при сборке под Windows.
fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let numeric = numeric_version(&version);
    let packed = packed_version(&numeric);
    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("../../assets/icon.ico")
        .set("CompanyName", "Freekazoid")
        .set("ProductName", "Screen Recorder")
        .set("FileDescription", "Screen Recorder")
        .set("InternalName", "screenrecorder")
        .set("OriginalFilename", "screenrecorder.exe")
        .set("LegalCopyright", "Copyright (c) 2026 Freekazoid")
        .set("FileVersion", &numeric)
        .set("ProductVersion", &version)
        .set_version_info(winresource::VersionInfo::FILEVERSION, packed)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    resource.compile().expect("Windows resource");
}

/// Превращает `4.0.0-alpha.3` в четырёхчастную версию `4.0.0.3`.
fn numeric_version(version: &str) -> String {
    let base = version.split('-').next().unwrap_or(version);
    let mut parts: Vec<u32> = base
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect();
    while parts.len() < 3 {
        parts.push(0);
    }
    let build = version
        .rsplit('.')
        .next()
        .and_then(|part| part.parse().ok())
        .unwrap_or(0);
    format!(
        "{}.{}.{}.{}",
        parts[0].min(65535),
        parts[1].min(65535),
        parts[2].min(65535),
        build.min(65535)
    )
}

/// Упаковывает четыре числа в 64-битное значение VERSIONINFO.
fn packed_version(numeric: &str) -> u64 {
    let mut parts = numeric
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0).min(65535));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    let build = parts.next().unwrap_or(0);
    (major << 48) | (minor << 32) | (patch << 16) | build
}
