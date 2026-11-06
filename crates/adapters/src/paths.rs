//! Кроссплатформенные каталоги приложения и проверка прав на запись.
//!
//! Ни один модуль приложения не должен писать в текущий каталог или в
//! каталог установки: все данные идут через этот сервис.

use directories::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Ошибки определения и проверки каталогов.
#[derive(Debug, Error)]
pub enum PathError {
    #[error("не удалось определить системные каталоги приложения")]
    NoProjectDirs,

    #[error("ошибка файловой системы {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("каталог недоступен для записи: {0}")]
    NotWritable(PathBuf),
}

/// Каталоги приложения, вычисленные для текущей ОС.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub default_output_dir: PathBuf,
}

impl Paths {
    /// Определяет каталоги по правилам ОС (XDG/AppData/Application Support).
    pub fn discover() -> Result<Self, PathError> {
        let project = ProjectDirs::from("io", "freekazoid", "ScreenRecorder")
            .ok_or(PathError::NoProjectDirs)?;

        let data_dir = project.data_dir().to_path_buf();
        let config_dir = project.config_dir().to_path_buf();
        let cache_dir = project.cache_dir().to_path_buf();
        let log_dir = data_dir.join("logs");
        let temp_dir = cache_dir.join("tmp");
        let default_output_dir = dirs::video_dir()
            .unwrap_or_else(|| data_dir.join("exports"))
            .join("ScreenRecorder");

        Ok(Self {
            data_dir,
            config_dir,
            cache_dir,
            log_dir,
            temp_dir,
            default_output_dir,
        })
    }

    /// Путь к пользовательскому файлу настроек.
    pub fn settings_path(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    /// Каталог моделей.
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    /// Каталог конкретной сессии записи.
    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.data_dir.join("recordings").join(session_id)
    }

    /// Создаёт все основные каталоги приложения.
    pub fn ensure_all(&self) -> Result<(), PathError> {
        for dir in [
            &self.data_dir,
            &self.config_dir,
            &self.cache_dir,
            &self.log_dir,
            &self.temp_dir,
        ] {
            self.ensure_dir(dir)?;
        }
        Ok(())
    }

    /// Проверяет, что каталог можно создать и в него можно записать файл.
    pub fn ensure_writable(&self, path: &Path) -> Result<(), PathError> {
        self.ensure_dir(path)?;
        let probe = path.join(".write_probe");
        fs::write(&probe, b"ok").map_err(|source| PathError::Io {
            path: probe.clone(),
            source,
        })?;
        fs::remove_file(&probe).map_err(|source| PathError::Io {
            path: probe,
            source,
        })
    }

    /// Создаёт каталог, если его ещё нет.
    fn ensure_dir(&self, path: &Path) -> Result<(), PathError> {
        fs::create_dir_all(path).map_err(|source| PathError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет создание и запись во временный каталог.
    #[test]
    fn ensures_writable_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths {
            data_dir: dir.path().to_path_buf(),
            config_dir: dir.path().to_path_buf(),
            cache_dir: dir.path().to_path_buf(),
            log_dir: dir.path().join("logs"),
            temp_dir: dir.path().join("tmp"),
            default_output_dir: dir.path().join("out"),
        };
        paths.ensure_all().expect("ensure_all");
        paths
            .ensure_writable(&dir.path().join("checks"))
            .expect("каталог должен быть доступен для записи");
    }
}
