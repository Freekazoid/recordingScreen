//! Менеджер моделей: скачивание GGML/ONNX-файлов с докачкой.
//!
//! Модели лежат в `<data>/models/<model>/<file>`. Прерванная загрузка
//! сохраняется в `.part` и продолжается по HTTP Range.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Ошибки скачивания моделей.
#[derive(Debug, Error)]
pub enum ModelError {
    #[error("неизвестная модель: {0}")]
    UnknownModel(String),

    #[error("ошибка сети: {0}")]
    Http(String),

    #[error("ошибка файла {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("контрольная сумма {path}: ожидалось {expected}, получено {actual}")]
    Checksum {
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

/// Один файл модели.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub url: &'static str,
    pub file_name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// Описание модели.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub files: &'static [ModelFile],
}

/// Доступные модели.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "whisper",
        title: "Whisper large-v3-turbo",
        files: &[ModelFile {
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
            file_name: "ggml-large-v3-turbo-q5_0.bin",
            size: 574_041_195,
            sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        }],
    },
    ModelSpec {
        id: "diarization",
        title: "Диаризация sherpa-onnx",
        files: &[
            ModelFile {
                url: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx",
                file_name: "segmentation.onnx",
                size: 5_992_913,
                sha256: "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079",
            },
            ModelFile {
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
                file_name: "embedding.onnx",
                size: 39_593_761,
                sha256: "1a331345f04805badbb495c775a6ddffcdd1a732567d5ec8b3d5749e3c7a5e4b",
            },
        ],
    },
    ModelSpec {
        id: "vad",
        title: "Silero VAD (GGML)",
        files: &[ModelFile {
            url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin",
            file_name: "ggml-silero-v5.1.2.bin",
            size: 885_098,
            sha256: "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
        }],
    },
];

/// Прогресс загрузки.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub model_id: String,
    pub file: String,
    pub downloaded: u64,
    pub total: u64,
    pub percent: f32,
}

/// Менеджер моделей в каталоге данных приложения.
#[derive(Debug, Clone)]
pub struct ModelManager {
    root: PathBuf,
}

impl ModelManager {
    /// Создаёт менеджер для каталога моделей.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Возвращает описание модели по идентификатору.
    pub fn spec(id: &str) -> Option<&'static ModelSpec> {
        MODELS.iter().find(|spec| spec.id == id)
    }

    /// Проверяет, что все файлы модели уже скачаны.
    pub fn is_downloaded(&self, id: &str) -> bool {
        let Some(spec) = Self::spec(id) else {
            return false;
        };
        spec.files.iter().all(|file| {
            let path = self.file_path(spec, file);
            fs::metadata(path)
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false)
        })
    }

    /// Скачивает модель и сообщает прогресс через callback.
    pub fn download(
        &self,
        id: &str,
        mut progress: impl FnMut(DownloadProgress),
    ) -> Result<(), ModelError> {
        let spec = Self::spec(id).ok_or_else(|| ModelError::UnknownModel(id.to_string()))?;
        let total: u64 = spec.files.iter().map(|file| file.size).sum();
        let mut done: u64 = 0;

        for file in spec.files {
            let path = self.file_path(spec, file);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| ModelError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            if path.exists() {
                if verify_sha256(&path, file.sha256).is_ok() {
                    done += file.size;
                    continue;
                }
                let _ = fs::remove_file(&path);
            }

            let part = path.with_extension(format!(
                "{}.part",
                path.extension()
                    .and_then(|ext| ext.to_str())
                    .unwrap_or("download")
            ));
            let offset = fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
            if offset >= file.size && file.size > 0 {
                fs::rename(&part, &path).map_err(|source| ModelError::Io {
                    path: path.clone(),
                    source,
                })?;
                verify_sha256(&path, file.sha256)?;
                done += file.size;
                continue;
            }

            let mut request = ureq::get(file.url);
            if offset > 0 {
                request = request.set("Range", &format!("bytes={offset}-"));
            }
            let response = request
                .call()
                .map_err(|error| ModelError::Http(error.to_string()))?;
            let status = response.status();
            let append = offset > 0 && status == 206;
            let mut output = if append {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&part)
                    .map_err(|source| ModelError::Io {
                        path: part.clone(),
                        source,
                    })?
            } else {
                fs::File::create(&part).map_err(|source| ModelError::Io {
                    path: part.clone(),
                    source,
                })?
            };

            let mut reader = response.into_reader();
            let mut buffer = [0u8; 64 * 1024];
            let mut current = if append { offset } else { 0 };
            loop {
                let read = reader.read(&mut buffer).map_err(|source| ModelError::Io {
                    path: part.clone(),
                    source,
                })?;
                if read == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..read])
                    .map_err(|source| ModelError::Io {
                        path: part.clone(),
                        source,
                    })?;
                current += read as u64;
                progress(DownloadProgress {
                    model_id: spec.id.to_string(),
                    file: file.file_name.to_string(),
                    downloaded: done + current,
                    total,
                    percent: if total == 0 {
                        0.0
                    } else {
                        (done + current) as f32 / total as f32 * 100.0
                    },
                });
            }
            output.flush().map_err(|source| ModelError::Io {
                path: part.clone(),
                source,
            })?;
            drop(output);
            fs::rename(&part, &path).map_err(|source| ModelError::Io {
                path: path.clone(),
                source,
            })?;
            verify_sha256(&path, file.sha256)?;
            done += file.size;
        }

        Ok(())
    }

    /// Проверяет SHA256 файла модели.
    pub fn verify(&self, id: &str) -> Result<(), ModelError> {
        let spec = Self::spec(id).ok_or_else(|| ModelError::UnknownModel(id.to_string()))?;
        for file in spec.files {
            let path = self.file_path(spec, file);
            verify_sha256(&path, file.sha256)?;
        }
        Ok(())
    }

    /// Путь к файлу модели.
    fn file_path(&self, spec: &ModelSpec, file: &ModelFile) -> PathBuf {
        self.root.join(spec.id).join(file.file_name)
    }
}

/// Считает SHA256 и сравнивает с ожидаемым.
fn verify_sha256(path: &Path, expected: &str) -> Result<(), ModelError> {
    let mut file = fs::File::open(path).map_err(|source| ModelError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| ModelError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    if actual != expected {
        return Err(ModelError::Checksum {
            path: path.to_path_buf(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет, что неизвестная модель не считается скачанной.
    #[test]
    fn unknown_model_is_not_downloaded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(dir.path());
        assert!(!manager.is_downloaded("unknown"));
    }

    /// Проверяет определение готовности по файлам.
    #[test]
    fn detects_downloaded_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(dir.path());
        let spec = ModelManager::spec("vad").expect("vad");
        let path = manager.file_path(spec, &spec.files[0]);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, b"test").expect("write");
        assert!(manager.is_downloaded("vad"));
    }

    /// Проверяет совпадение и несовпадение SHA256.
    #[test]
    fn verifies_sha256() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.bin");
        fs::write(&path, b"test").expect("write");
        verify_sha256(
            &path,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
        )
        .expect("hash");
        assert!(verify_sha256(&path, "00").is_err());
    }

    /// Реальное скачивание маленькой VAD-модели; запускать вручную.
    #[test]
    #[ignore = "требует сеть"]
    fn downloads_vad_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = ModelManager::new(dir.path());
        manager.download("vad", |_| {}).expect("скачивание");
        assert!(manager.is_downloaded("vad"));
    }
}
