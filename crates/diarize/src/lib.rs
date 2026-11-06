//! Диаризация sherpa-onnx в отдельном процессе.
//!
//! Whisper.cpp и sherpa-onnx нельзя линковать в один процесс: их нативные
//! библиотеки конфликтуют. Поэтому диаризация живёт в отдельном бинарнике
//! `screenrecorder-diarize`.

use serde::{Deserialize, Serialize};
use sherpa_onnx_sys::offline_speaker_diarization as sys;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Ошибки диаризации.
#[derive(Debug, Error)]
pub enum DiarizeError {
    #[error("модель недоступна: {0}")]
    ModelUnavailable(String),

    #[error("аудио: {0}")]
    Audio(String),

    #[error("sherpa-onnx: {0}")]
    Inference(String),
}

/// Реплика спикера.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: String,
}

/// Движок диаризации.
pub struct SherpaDiarizationEngine {
    segmentation: PathBuf,
    embedding: PathBuf,
    threads: i32,
}

impl SherpaDiarizationEngine {
    /// Создаёт движок по путям к моделям.
    pub fn new(segmentation: impl Into<PathBuf>, embedding: impl Into<PathBuf>) -> Self {
        Self {
            segmentation: segmentation.into(),
            embedding: embedding.into(),
            threads: 4,
        }
    }

    /// Проверяет наличие обеих моделей.
    pub fn is_ready(&self) -> bool {
        self.segmentation.is_file() && self.embedding.is_file()
    }

    /// Размечает спикеров в WAV-файле.
    pub fn diarize(
        &self,
        audio_path: &Path,
        min_speakers: Option<u32>,
        max_speakers: Option<u32>,
    ) -> Result<Vec<SpeakerTurn>, DiarizeError> {
        if !self.is_ready() {
            return Err(DiarizeError::ModelUnavailable(format!(
                "{} / {}",
                self.segmentation.display(),
                self.embedding.display()
            )));
        }
        let segmentation = cstring(&self.segmentation)?;
        let embedding = cstring(&self.embedding)?;
        let cpu =
            CString::new("cpu").map_err(|error| DiarizeError::Inference(error.to_string()))?;
        let mut config: sys::OfflineSpeakerDiarizationConfig = unsafe { std::mem::zeroed() };
        config.segmentation.pyannote.model = segmentation.as_ptr();
        config.segmentation.pyannote.window_shift_ratio = 0.1;
        config.segmentation.num_threads = self.threads;
        config.segmentation.debug = 0;
        config.segmentation.provider = cpu.as_ptr();
        config.embedding.model = embedding.as_ptr();
        config.embedding.num_threads = self.threads;
        config.embedding.debug = 0;
        config.embedding.provider = cpu.as_ptr();
        config.clustering.num_clusters = exact_speakers(min_speakers, max_speakers);
        config.clustering.threshold = 0.5;
        config.clustering.compute_confidence = 0;
        config.min_duration_on = 0.3;
        config.min_duration_off = 0.5;

        let diarizer = unsafe { sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config) };
        if diarizer.is_null() {
            return Err(DiarizeError::Inference(
                "не удалось создать диаризатор".to_string(),
            ));
        }
        let guard = DiarizerGuard(diarizer);
        let expected = unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationGetSampleRate(diarizer) };
        let mut samples = load_wav_16k_mono(audio_path)?;
        if samples.is_empty() {
            return Err(DiarizeError::Audio("аудиофайл пуст".to_string()));
        }
        if expected > 0 && expected != 16_000 {
            samples = resample_linear(&samples, 16_000, expected as u32);
        }
        let raw = unsafe {
            sys::SherpaOnnxOfflineSpeakerDiarizationProcess(
                diarizer,
                samples.as_ptr(),
                samples.len() as i32,
            )
        };
        if raw.is_null() {
            return Err(DiarizeError::Inference(
                "диаризация не вернула результат".to_string(),
            ));
        }
        let result = ResultGuard(raw);
        let count =
            unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result.0) }
                .max(0);
        let segments_ptr =
            unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result.0) };
        if segments_ptr.is_null() || count == 0 {
            return Ok(Vec::new());
        }
        let segments = unsafe { std::slice::from_raw_parts(segments_ptr, count as usize) };
        let turns = segments
            .iter()
            .map(|segment| SpeakerTurn {
                start_ms: (segment.start.max(0.0) * 1000.0) as u64,
                end_ms: (segment.end.max(0.0) * 1000.0) as u64,
                speaker: format!("спикер_{}", segment.speaker.max(0) + 1),
            })
            .collect();
        unsafe {
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(segments_ptr);
        }
        drop(result);
        drop(guard);
        Ok(turns)
    }
}

/// Владелец указателя диаризатора.
struct DiarizerGuard(*const sys::OfflineSpeakerDiarization);

impl Drop for DiarizerGuard {
    /// Освобождает диаризатор.
    fn drop(&mut self) {
        unsafe { sys::SherpaOnnxDestroyOfflineSpeakerDiarization(self.0) };
    }
}

/// Владелец указателя результата.
struct ResultGuard(*const sys::OfflineSpeakerDiarizationResult);

impl Drop for ResultGuard {
    /// Освобождает результат.
    fn drop(&mut self) {
        unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(self.0) };
    }
}

/// Преобразует путь в C-строку.
fn cstring(path: &Path) -> Result<CString, DiarizeError> {
    CString::new(path.display().to_string())
        .map_err(|error| DiarizeError::Inference(format!("путь модели: {error}")))
}

/// Возвращает точное число кластеров, если диапазон схлопнут.
fn exact_speakers(min: Option<u32>, max: Option<u32>) -> i32 {
    match (min, max) {
        (Some(min), Some(max)) if min == max => max as i32,
        _ => -1,
    }
}

/// Читает WAV и приводит к 16 кГц mono.
fn load_wav_16k_mono(path: &Path) -> Result<Vec<f32>, DiarizeError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| DiarizeError::Audio(format!("WAV {}: {error}", path.display())))?;
    let spec = reader.spec();
    let mut mono = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => {
            let mut values = Vec::with_capacity(reader.len() as usize);
            for sample in reader.samples::<i16>() {
                values.push(
                    sample.map_err(|error| DiarizeError::Audio(format!("сэмпл: {error}")))? as f32
                        / 32768.0,
                );
            }
            values
        }
        (hound::SampleFormat::Float, 32) => {
            let mut values = Vec::with_capacity(reader.len() as usize);
            for sample in reader.samples::<f32>() {
                values
                    .push(sample.map_err(|error| DiarizeError::Audio(format!("сэмпл: {error}")))?);
            }
            values
        }
        _ => {
            return Err(DiarizeError::Audio(
                "ожидается WAV PCM s16 или f32".to_string(),
            ));
        }
    };
    if spec.channels == 0 || spec.sample_rate == 0 {
        return Err(DiarizeError::Audio("некорректный формат WAV".to_string()));
    }
    if spec.channels > 1 {
        let channels = spec.channels as usize;
        let mut mixed = Vec::with_capacity(mono.len() / channels);
        for frame in mono.chunks_exact(channels) {
            mixed.push(frame.iter().sum::<f32>() / channels as f32);
        }
        mono = mixed;
    }
    if spec.sample_rate != 16_000 {
        mono = resample_linear(&mono, spec.sample_rate, 16_000);
    }
    Ok(mono)
}

/// Линейный ресемплинг с усреднением при кратном понижении.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let mut source = input;
    let mut filtered;
    let mut effective_from = from;
    if from > to && from.is_multiple_of(to) {
        let factor = (from / to) as usize;
        if factor > 1 {
            filtered = Vec::with_capacity(input.len() / factor);
            for chunk in input.chunks_exact(factor) {
                filtered.push(chunk.iter().sum::<f32>() / factor as f32);
            }
            source = &filtered;
            effective_from = from / factor as u32;
        }
    }
    let ratio = effective_from as f64 / to as f64;
    let out_len = ((source.len() as f64) / ratio).round() as usize;
    let mut output = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let fraction = (position - left as f64) as f32;
        let first = source.get(left).copied().unwrap_or(0.0);
        let second = source.get(left + 1).copied().unwrap_or(first);
        output.push(first + (second - first) * fraction);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет точное число спикеров.
    #[test]
    fn chooses_exact_cluster_count() {
        assert_eq!(exact_speakers(Some(2), Some(2)), 2);
        assert_eq!(exact_speakers(None, Some(3)), -1);
    }

    /// Проверяет ресемплинг 48 кГц → 16 кГц.
    #[test]
    fn resamples_audio() {
        let input = vec![0.1_f32; 48_000];
        let output = resample_linear(&input, 48_000, 16_000);
        assert!((output.len() as i64 - 16_000).abs() <= 1);
    }
}
