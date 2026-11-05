//! ASR на whisper-rs (whisper.cpp).

use crate::audio::load_wav_16k_mono;
use crate::{MlError, Transcript, TranscriptSegment};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext,
    WhisperVadContextParams, WhisperVadParams, get_lang_str,
};

/// Пишет прогресс ML-задачи рядом с аудиофайлом.
///
/// UI читает этот файл и показывает процент, а также PID для отмены.
pub fn write_progress_file(audio: &Path, stage: &str, percent: u8) {
    write_progress_file_with_pass(audio, stage, percent, 0);
}

/// Пишет прогресс с номером прохода Whisper (1 — system, 2 — mic).
pub fn write_progress_file_with_pass(audio: &Path, stage: &str, percent: u8, pass: u8) {
    write_progress_file_with_device(audio, stage, percent, pass, "");
}

/// Пишет прогресс с указанием фактического устройства (cpu/gpu).
pub fn write_progress_file_with_device(
    audio: &Path,
    stage: &str,
    percent: u8,
    pass: u8,
    device: &str,
) {
    let Some(dir) = audio.parent() else {
        return;
    };
    let path = dir.join("progress.json");
    let temp = dir.join("progress.json.tmp");
    let data = format!(
        r#"{{"stage":"{stage}","percent":{},"pid":{},"pass":{},"device":"{device}"}}"#,
        percent.min(100),
        std::process::id(),
        pass
    );
    if std::fs::write(&temp, data).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// Язык по локали системы, если auto-определение неуверенно.
fn fallback_language() -> String {
    let locale = std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .unwrap_or_default()
        .to_lowercase();
    if locale.starts_with("ru") {
        "ru".to_string()
    } else if locale.starts_with("uk") {
        "uk".to_string()
    } else {
        "en".to_string()
    }
}

/// Мягко нормализует громкость тихих дорожек перед VAD и Whisper.
fn normalize_samples(samples: &[f32]) -> Vec<f32> {
    let count = samples.len().max(1) as f64;
    let sum: f64 = samples
        .iter()
        .map(|value| (*value as f64) * (*value as f64))
        .sum();
    let rms = (sum / count).sqrt() as f32;
    if rms < 1e-6 {
        return samples.to_vec();
    }
    let rms_db = 20.0 * rms.log10();
    let gain_db = if rms_db < -30.0 {
        (-22.0 - rms_db).clamp(0.0, 24.0)
    } else {
        0.0
    };
    if gain_db < 0.1 {
        return samples.to_vec();
    }
    let gain = 10.0_f32.powf(gain_db / 20.0);
    let peak = samples.iter().map(|value| value.abs()).fold(0.0, f32::max) * gain;
    let limit = if peak > 0.95 { 0.95 / peak } else { 1.0 };
    samples.iter().map(|value| value * gain * limit).collect()
}

/// Отбрасывает типичные галлюцинации Whisper на тишине.
///
/// Короткое настоящее «спасибо» не трогаем; фильтр срабатывает только
/// для длинных (≥10 с) сегментов с boilerplate-фразами.
fn is_hallucination(text: &str, start_cs: i64, end_cs: i64) -> bool {
    let duration_ms = (end_cs.saturating_sub(start_cs).max(0) as u64) * 10;
    if duration_ms < 10_000 {
        return false;
    }
    let normalized: String = text
        .to_lowercase()
        .chars()
        .filter(|character| character.is_alphanumeric() || character.is_whitespace())
        .collect();
    let normalized = normalized.trim();
    const BOILERPLATE: &[&str] = &[
        "thank you",
        "thanks for watching",
        "thank you for watching",
        "продолжение следует",
        "спасибо за просмотр",
        "подписывайтесь",
        "субтитры",
        "до встречи",
    ];
    BOILERPLATE
        .iter()
        .any(|phrase| normalized == *phrase || normalized.starts_with(&format!("{phrase} ")))
}

/// Проверяет, доступен ли CUDA-GPU в этой сборке и системе.
pub fn gpu_available() -> bool {
    if !cfg!(feature = "cuda") {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/dev/nvidia0").exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Движок распознавания речи с кэшем загруженной модели.
pub struct WhisperEngine {
    model_path: PathBuf,
    vad_path: Option<PathBuf>,
    context: Option<WhisperContext>,
    vad_context: Option<WhisperVadContext>,
    threads: i32,
    use_gpu: bool,
}

impl WhisperEngine {
    /// Создаёт движок; модель загружается при первом запросе.
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self::with_vad(model_path, None)
    }

    /// Создаёт движок с опциональной моделью Silero VAD.
    pub fn with_vad(model_path: impl Into<PathBuf>, vad_path: Option<PathBuf>) -> Self {
        Self::with_vad_and_device(model_path, vad_path, false)
    }

    /// Создаёт движок с выбором CPU/GPU.
    pub fn with_vad_and_device(
        model_path: impl Into<PathBuf>,
        vad_path: Option<PathBuf>,
        use_gpu: bool,
    ) -> Self {
        let threads = std::thread::available_parallelism()
            .map(|count| count.get().min(8) as i32)
            .unwrap_or(4);
        Self {
            model_path: model_path.into(),
            vad_path,
            context: None,
            vad_context: None,
            threads,
            use_gpu: use_gpu && cfg!(feature = "cuda"),
        }
    }

    /// Использует ли движок GPU.
    pub fn uses_gpu(&self) -> bool {
        self.use_gpu
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
    ///
    /// Сначала вырезает речь через Silero VAD, затем распознаёт только
    /// речевые интервалы. Это убирает галлюцинации Whisper на тишине
    /// («Thank you», «Продолжение следует» и т.п.).
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
        let samples = normalize_samples(samples);
        if samples.is_empty() {
            return Err(MlError::Inference("аудиофайл пуст".to_string()));
        }
        let ranges = self.speech_ranges(&samples)?;
        if ranges.is_empty() {
            progress(100);
            return Ok(Transcript {
                language: language.unwrap_or("auto").to_string(),
                segments: Vec::new(),
            });
        }
        let threads = self.threads;
        let mut locked_language = language
            .filter(|value| *value != "auto")
            .map(str::to_string);
        if locked_language.is_none() {
            locked_language = self.detect_language(&samples, &ranges, threads)?;
        }
        if locked_language.is_none() {
            locked_language = Some(fallback_language());
        }
        let detected_language = locked_language
            .clone()
            .unwrap_or_else(|| "auto".to_string());
        let context = self.context()?;
        let total = ranges.len().max(1) as f32;
        let mut segments = Vec::new();
        for (index, (start, end)) in ranges.iter().enumerate() {
            let slice = &samples[*start..*end];
            let mut state = context
                .create_state()
                .map_err(|error| MlError::Inference(format!("состояние whisper: {error}")))?;
            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_n_threads(threads);
            params.set_no_context(true);
            params.set_suppress_blank(true);
            params.set_suppress_nst(true);
            params.set_temperature(0.0);
            params.set_temperature_inc(0.0);
            params.set_translate(false);
            params.set_language(locked_language.as_deref());
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_token_timestamps(timestamps);
            let callback = Arc::clone(&progress);
            params.set_progress_callback_safe(move |value: i32| {
                let phase = index as f32;
                let overall = ((phase + value.clamp(0, 100) as f32 / 100.0) / total * 100.0)
                    .clamp(0.0, 100.0);
                callback(overall as u8);
            });
            state
                .full(params, slice)
                .map_err(|error| MlError::Inference(format!("whisper: {error}")))?;
            let offset_ms = (*start as u64) * 1000 / 16_000;
            let mut kept_in_range = false;
            for segment in state.as_iter() {
                let text = segment
                    .to_str_lossy()
                    .map_err(|error| MlError::Inference(format!("текст сегмента: {error}")))?
                    .trim()
                    .to_string();
                if text.is_empty()
                    || is_hallucination(&text, segment.start_timestamp(), segment.end_timestamp())
                {
                    continue;
                }
                let start_ms = (segment.start_timestamp().max(0) * 10) as u64 + offset_ms;
                let end_ms = (segment.end_timestamp().max(0) * 10) as u64 + offset_ms;
                segments.push(TranscriptSegment {
                    start_ms,
                    end_ms: end_ms.max(start_ms),
                    text,
                    speaker: None,
                });
                kept_in_range = true;
            }
            let _ = kept_in_range;
        }
        progress(100);
        Ok(Transcript {
            language: detected_language,
            segments,
        })
    }

    /// Определяет язык по первым ~30 секундам речи с проверкой уверенности.
    fn detect_language(
        &mut self,
        samples: &[f32],
        ranges: &[(usize, usize)],
        threads: i32,
    ) -> Result<Option<String>, MlError> {
        let mut probe = Vec::with_capacity(30 * 16_000);
        for (start, end) in ranges {
            let remaining = 30 * 16_000 - probe.len();
            if remaining == 0 {
                break;
            }
            let take = (*end - *start).min(remaining);
            probe.extend_from_slice(&samples[*start..*start + take]);
        }
        if probe.len() < 16_000 {
            return Ok(None);
        }
        let context = self.context()?;
        let mut state = context
            .create_state()
            .map_err(|error| MlError::Inference(format!("состояние whisper: {error}")))?;
        state
            .pcm_to_mel(&probe, threads.max(1) as usize)
            .map_err(|error| MlError::Inference(format!("язык: {error}")))?;
        let (id, probabilities) = state
            .lang_detect(0, threads.max(1) as usize)
            .map_err(|error| MlError::Inference(format!("определение языка: {error}")))?;
        let top = probabilities.get(id as usize).copied().unwrap_or(0.0);
        let second = probabilities
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != id as usize)
            .map(|(_, value)| *value)
            .fold(0.0_f32, f32::max);
        if top < 0.5 || top < second * 1.2 {
            return Ok(None);
        }
        Ok(get_lang_str(id).map(str::to_string))
    }

    /// Возвращает интервалы речи по Silero VAD; при отсутствии модели — весь файл.
    fn speech_ranges(&mut self, samples: &[f32]) -> Result<Vec<(usize, usize)>, MlError> {
        let Some(path) = self.vad_path.clone().filter(|path| path.is_file()) else {
            return Ok(vec![(0, samples.len())]);
        };
        if self.vad_context.is_none() {
            let model = path.to_string_lossy().to_string();
            let context = WhisperVadContext::new(&model, WhisperVadContextParams::default())
                .map_err(|error| MlError::Inference(format!("VAD {model}: {error}")))?;
            self.vad_context = Some(context);
        }
        let mut params = WhisperVadParams::default();
        params.set_threshold(0.65);
        params.set_min_speech_duration(400);
        params.set_min_silence_duration(500);
        params.set_speech_pad(300);
        params.set_max_speech_duration(28.0);
        let context = self
            .vad_context
            .as_mut()
            .ok_or_else(|| MlError::Inference("VAD не загружен".to_string()))?;
        let detected = context
            .segments_from_samples(params, samples)
            .map_err(|error| MlError::Inference(format!("VAD: {error}")))?;
        let mut ranges = Vec::new();
        for segment in detected {
            // VAD отдаёт сотые доли секунды.
            let start = (segment.start * 160.0).max(0.0) as usize;
            let end = (segment.end * 160.0).max(0.0) as usize;
            let start = start.min(samples.len());
            let end = end.min(samples.len());
            if end > start + 1_600 {
                ranges.push((start, end));
            }
        }
        ranges.sort_unstable();
        // Склеиваем близкие фразы, чтобы Whisper видел предложение, а не
        // отдельные слова/названия.
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (start, end) in ranges {
            if let Some((first, last_end)) = merged.last_mut()
                && start <= *last_end + 24_000
                && end.saturating_sub(*first) <= 28 * 16_000
            {
                *last_end = (*last_end).max(end);
            } else {
                merged.push((start, end));
            }
        }
        Ok(merged)
    }

    /// Загружает контекст при первом использовании.
    fn context(&mut self) -> Result<&WhisperContext, MlError> {
        if self.context.is_none() {
            let path = self.model_path.to_string_lossy().to_string();
            let mut params = WhisperContextParameters::default();
            params.use_gpu(self.use_gpu);
            let context = WhisperContext::new_with_params(&path, params).map_err(|error| {
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
