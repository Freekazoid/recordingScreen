//! ASR на whisper-rs (whisper.cpp).

use crate::audio::load_wav_16k_mono;
use crate::{MlError, Transcript, TranscriptSegment};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadParams,
};

/// Пишет прогресс ML-задачи рядом с аудиофайлом.
///
/// UI читает этот файл и показывает процент, а также PID для отмены.
pub fn write_progress_file(audio: &Path, stage: &str, percent: u8) {
    let Some(dir) = audio.parent() else {
        return;
    };
    let path = dir.join("progress.json");
    let temp = dir.join("progress.json.tmp");
    let data = format!(
        r#"{{"stage":"{stage}","percent":{},"pid":{}}}"#,
        percent.min(100),
        std::process::id()
    );
    if std::fs::write(&temp, data).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// Движок распознавания речи с кэшем загруженной модели.
pub struct WhisperEngine {
    model_path: PathBuf,
    vad_path: Option<PathBuf>,
    context: Option<WhisperContext>,
    threads: i32,
}

impl WhisperEngine {
    /// Создаёт движок; модель загружается при первом запросе.
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self::with_vad(model_path, None)
    }

    /// Создаёт движок с опциональной моделью Silero VAD.
    pub fn with_vad(model_path: impl Into<PathBuf>, vad_path: Option<PathBuf>) -> Self {
        let threads = std::thread::available_parallelism()
            .map(|count| count.get().min(8) as i32)
            .unwrap_or(4);
        Self {
            model_path: model_path.into(),
            vad_path,
            context: None,
            threads,
        }
    }

    /// Проверяет, что файл модели существует.
    pub fn is_ready(&self) -> bool {
        self.model_path.is_file()
    }

    /// Распознаёт WAV-файл и возвращает сегменты с таймкодами.
    pub fn transcribe(
        &mut self,
        audio_path: &Path,
        language: Option<&str>,
        timestamps: bool,
    ) -> Result<Transcript, MlError> {
        self.transcribe_with_progress(audio_path, language, timestamps, |_| {})
    }

    /// Распознаёт WAV и сообщает прогресс в процентах.
    pub fn transcribe_with_progress<F>(
        &mut self,
        audio_path: &Path,
        language: Option<&str>,
        timestamps: bool,
        progress: F,
    ) -> Result<Transcript, MlError>
    where
        F: Fn(u8) + Send + Sync + 'static,
    {
        if !self.is_ready() {
            return Err(MlError::ModelUnavailable(
                self.model_path.display().to_string(),
            ));
        }
        let samples = load_wav_16k_mono(audio_path)?;
        if samples.is_empty() {
            return Err(MlError::Inference("аудиофайл пуст".to_string()));
        }
        self.transcribe_samples(&samples, language, timestamps, progress)
    }

    /// Распознаёт переданный PCM-буфер.
    fn transcribe_samples<F>(
        &mut self,
        samples: &[f32],
        language: Option<&str>,
        timestamps: bool,
        progress: F,
    ) -> Result<Transcript, MlError>
    where
        F: Fn(u8) + Send + Sync + 'static,
    {
        let progress = Arc::new(progress);
        let vad_path = self
            .vad_path
            .as_ref()
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().to_string());
        let context = self.context()?;
        let mut state = context
            .create_state()
            .map_err(|error| MlError::Inference(format!("состояние whisper: {error}")))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_no_context(true);
        if let Some(vad_path) = vad_path.as_deref() {
            params.set_vad_model_path(Some(vad_path));
            params.enable_vad(true);
            params.set_vad_params(WhisperVadParams::default());
        }
        params.set_translate(false);
        params.set_language(language.filter(|value| *value != "auto"));
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_token_timestamps(timestamps);
        let callback = Arc::clone(&progress);
        params.set_progress_callback_safe(move |value: i32| {
            callback(value.clamp(0, 100) as u8);
        });
        state
            .full(params, samples)
            .map_err(|error| MlError::Inference(format!("whisper: {error}")))?;

        let mut segments = Vec::new();
        for segment in state.as_iter() {
            let text = segment
                .to_str_lossy()
                .map_err(|error| MlError::Inference(format!("текст сегмента: {error}")))?
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            let start_ms = (segment.start_timestamp().max(0) * 10) as u64;
            let end_ms = (segment.end_timestamp().max(0) * 10) as u64;
            segments.push(TranscriptSegment {
                start_ms,
                end_ms: end_ms.max(start_ms),
                text,
                speaker: None,
            });
        }
        progress(100);
        Ok(Transcript {
            language: language.unwrap_or("auto").to_string(),
            segments,
        })
    }

    /// Загружает контекст при первом использовании.
    fn context(&mut self) -> Result<&WhisperContext, MlError> {
        if self.context.is_none() {
            let path = self.model_path.to_string_lossy().to_string();
            let context =
                WhisperContext::new_with_params(&path, WhisperContextParameters::default())
                    .map_err(|error| {
                        MlError::Inference(format!(
                            "загрузка модели {}: {error}",
                            self.model_path.display()
                        ))
                    })?;
            self.context = Some(context);
        }
        self.context
            .as_ref()
            .ok_or_else(|| MlError::Inference("модель не загружена".to_string()))
    }
}
