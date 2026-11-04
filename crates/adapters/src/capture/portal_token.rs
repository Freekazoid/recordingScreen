//! Хранение restore-token портала ScreenCast.
//!
//! Токен позволяет восстановить ранее выбранный источник без повторного
//! системного диалога. Портал выдаёт новый токен после каждого успешного
//! запуска, поэтому файл перезаписывается при каждом старте.

use super::PortalSource;
use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// Загружает сохранённый токен для источника.
pub(crate) fn load(source: PortalSource) -> Option<String> {
    let path = token_path(source)?;
    read_token(&path)
}

/// Сохраняет новый токен; пустой токен не записывается.
pub(crate) fn save(source: PortalSource, token: Option<&str>) {
    let Some(token) = token.map(str::trim).filter(|token| !token.is_empty()) else {
        return;
    };
    let Some(path) = token_path(source) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    write_token(&path, token);
}

/// Возвращает путь к файлу токена для источника.
fn token_path(source: PortalSource) -> Option<PathBuf> {
    let project = ProjectDirs::from("io", "freekazoid", "ScreenRecorder")?;
    let name = match source {
        PortalSource::Monitor => "screencast-monitor.token",
        PortalSource::Window => "screencast-window.token",
    };
    Some(project.config_dir().join(name))
}

/// Читает токен из файла, отбрасывая пустое содержимое.
fn read_token(path: &Path) -> Option<String> {
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Атомарно записывает токен с правами только для владельца.
fn write_token(path: &Path, token: &str) {
    let temp = path.with_extension("tmp");
    if std::fs::write(&temp, token).is_err() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&temp, path).is_err() {
        // На Windows rename не заменяет существующий файл.
        let _ = std::fs::remove_file(path);
        let _ = std::fs::rename(&temp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет чтение токена с обрезкой пробелов.
    #[test]
    fn reads_token_with_trimming() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        std::fs::write(&path, "  token-value\n").expect("запись токена");
        assert_eq!(read_token(&path).as_deref(), Some("token-value"));
    }

    /// Проверяет, что пустой файл не считается токеном.
    #[test]
    fn ignores_empty_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        std::fs::write(&path, "  \n").expect("запись пустого токена");
        assert_eq!(read_token(&path), None);
    }

    /// Проверяет атомарную запись и последующее чтение.
    #[test]
    fn writes_and_reads_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("token");
        write_token(&path, "next-token");
        assert_eq!(read_token(&path).as_deref(), Some("next-token"));
    }
}
