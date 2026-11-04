//! Диаризация NeMo Sortformer через ONNX.
//!
//! Вынесена в отдельный процесс: ONNX Runtime из parakeet-rs и статический
//! sherpa-onnx/whisper.cpp не должны жить в одном адресном пространстве.

use parakeet_rs::ExecutionConfig;
use parakeet_rs::sortformer::{DiarizationConfig, Sortformer};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Ошибки NeMo-диаризации.
#[derive(Debug, Error)]
pub enum NemoError {
    #[error("модель недоступна: {0}")]
    ModelUnavailable(String),

    #[error("аудио: {0}")]
    Audio(String),

    #[error("NeMo: {0}")]
    Inference(String),
}

/// Реплика спикера.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: String,
}

/// Движок NeMo Sortformer.
pub struct NemoDiarizer {
    model_path: PathBuf,
    diarizer: Option<Sortformer>,
}

impl NemoDiarizer {
    /// Создаёт движок по пути к ONNX-модели.
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        Self {
            model_path: model_path.into(),
            diarizer: None,
        }
    }

    /// Проверяет наличие модели.
    pub fn is_ready(&self) -> bool {
        self.model_path.is_file()
    }

    /// Размечает спикеров в WAV-файле.
    pub fn diarize(&mut self, audio_path: &Path) -> Result<Vec<SpeakerTurn>, NemoError> {
        if !self.is_ready() {
            return Err(NemoError::ModelUnavailable(
                self.model_path.display().to_string(),
            ));
        }
        let samples = load_wav_16k_mono(audio_path)?;
        if samples.is_empty() {
            return Err(NemoError::Audio("аудиофайл пуст".to_string()));
        }
        if self.diarizer.is_none() {
            let environment = parakeet_rs::ort::init()
                .build()
                .map_err(|error| NemoError::Inference(format!("ONNX Runtime: {error}")))?;
            let config = ExecutionConfig::new().with_environment(environment);
            let diarizer = Sortformer::with_config(
                &self.model_path,
                Some(config),
                DiarizationConfig::default(),
            )
            .map_err(|error| NemoError::Inference(error.to_string()))?;
            self.diarizer = Some(diarizer);
        }
        let diarizer = self
            .diarizer
            .as_mut()
            .ok_or_else(|| NemoError::Inference("диаризатор не создан".to_string()))?;
        let segments = diarizer
            .diarize(samples, 16_000, 1)
            .map_err(|error| NemoError::Inference(error.to_string()))?;
        Ok(segments
            .into_iter()
            .map(|segment| SpeakerTurn {
                start_ms: segment.start.saturating_mul(1000) / 16_000,
                end_ms: segment.end.saturating_mul(1000) / 16_000,
                speaker: format!("спикер_{}", segment.speaker_id + 1),
            })
            .collect())
    }
}

/// Читает WAV и приводит к 16 кГц mono.
fn load_wav_16k_mono(path: &Path) -> Result<Vec<f32>, NemoError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| NemoError::Audio(format!("WAV {}: {error}", path.display())))?;
    let spec = reader.spec();
    let mut mono = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => {
            let mut values = Vec::with_capacity(reader.len() as usize);
            for sample in reader.samples::<i16>() {
                values.push(
                    sample.map_err(|error| NemoError::Audio(format!("сэмпл: {error}")))? as f32
                        / 32768.0,
                );
            }
            values
        }
        (hound::SampleFormat::Float, 32) => {
            let mut values = Vec::with_capacity(reader.len() as usize);
            for sample in reader.samples::<f32>() {
                values.push(sample.map_err(|error| NemoError::Audio(format!("сэмпл: {error}")))?);
            }
            values
        }
        _ => {
            return Err(NemoError::Audio(
                "ожидается WAV PCM s16 или f32".to_string(),
            ));
        }
    };
    if spec.channels == 0 || spec.sample_rate == 0 {
        return Err(NemoError::Audio("некорректный формат WAV".to_string()));
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

    /// Проверяет ресемплинг 48 кГц → 16 кГц.
    #[test]
    fn resamples_audio() {
        let input = vec![0.1_f32; 48_000];
        let output = resample_linear(&input, 48_000, 16_000);
        assert!((output.len() as i64 - 16_000).abs() <= 1);
    }
}
