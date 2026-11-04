//! Менеджер моделей: скачивание GGML/ONNX-файлов с докачкой.
//!
//! Модели лежат в `<data>/models/<model>/<file>`. Прерванная загрузка
//! сохраняется в `.part` и продолжается по HTTP Range.

use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
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
}

/// Один файл модели.
#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub url: &'static str,
    pub file_name: &'static str,
    pub size: u64,
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
            },
            ModelFile {
                url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx",
                file_name: "embedding.onnx",
                size: 39_593_761,
            },
        ],
    },
    ModelSpec {
        id: "vad",
        title: "Silero VAD",
        files: &[ModelFile {
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
            file_name: "silero_vad.onnx",
            size: 643_854,
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
                done += file.size;
                continue;
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
            done += file.size;
        }

        Ok(())
    }

    /// Путь к файлу модели.
    fn file_path(&self, spec: &ModelSpec, file: &ModelFile) -> PathBuf {
        self.root.join(spec.id).join(file.file_name)
    }
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
