//! Загрузка WAV и приведение к 16 кГц mono f32 для Whisper.

use crate::MlError;
use std::path::Path;

/// Читает WAV и возвращает mono-сигнал 16 кГц.
pub fn load_wav_16k_mono(path: &Path) -> Result<Vec<f32>, MlError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|error| MlError::Inference(format!("WAV {}: {error}", path.display())))?;
    let spec = reader.spec();
    let mut mono = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => {
            let mut values = Vec::with_capacity(reader.len() as usize);
            for sample in reader.samples::<i16>() {
                values.push(
                    sample.map_err(|error| MlError::Inference(format!("сэмпл: {error}")))? as f32
                        / 32768.0,
                );
            }
            values
        }
        (hound::SampleFormat::Float, 32) => {
            let mut values = Vec::with_capacity(reader.len() as usize);
            for sample in reader.samples::<f32>() {
                values.push(sample.map_err(|error| MlError::Inference(format!("сэмпл: {error}")))?);
            }
            values
        }
        _ => {
            return Err(MlError::Inference(
                "ожидается WAV PCM s16 или f32".to_string(),
            ));
        }
    };
    if spec.channels == 0 || spec.sample_rate == 0 {
        return Err(MlError::Inference("некорректный формат WAV".to_string()));
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

/// Линейный ресемплинг с простым усреднением при кратном понижении.
pub fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
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

    /// Проверяет длину после понижения 48 кГц → 16 кГц.
    #[test]
    fn resamples_48k_to_16k() {
        let input: Vec<f32> = (0..48_000)
            .map(|index| (index as f32 / 48_000.0).sin())
            .collect();
        let output = resample_linear(&input, 48_000, 16_000);
        assert!((output.len() as i64 - 16_000).abs() <= 1);
    }

    /// Проверяет, что частота не меняется без необходимости.
    #[test]
    fn keeps_16k_unchanged() {
        let input = vec![0.1_f32; 16_000];
        let output = resample_linear(&input, 16_000, 16_000);
        assert_eq!(output, input);
    }
}
