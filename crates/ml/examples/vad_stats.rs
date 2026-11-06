//! Временная диагностика: сколько речевых интервалов и пауз находит Silero VAD.
//!
//! Запуск:
//! `cargo run -p screenrecorder-ml --example vad_stats -- <16k mono wav> <vad model>`

use std::path::Path;

use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

/// Загружает 16 кГц mono WAV в f32.
fn load_wav(path: &Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("WAV");
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "нужен mono");
    assert_eq!(spec.sample_rate, 16_000, "нужен 16 кГц");
    match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .map(|s| s.unwrap() as f32 / 32768.0)
            .collect(),
    }
}

/// Печатает распределение пауз между речевыми интервалами.
fn print_gaps(raw: &[(f32, f32)]) {
    let mut buckets = [0usize; 6];
    let mut total = 0.0f32;
    let mut count = 0usize;
    for pair in raw.windows(2) {
        let gap = (pair[1].0 - pair[0].1).max(0.0) * 10.0;
        if gap <= 0.0 {
            continue;
        }
        total += gap;
        count += 1;
        let index = if gap < 200.0 {
            0
        } else if gap < 500.0 {
            1
        } else if gap < 1_000.0 {
            2
        } else if gap < 1_500.0 {
            3
        } else if gap < 3_000.0 {
            4
        } else {
            5
        };
        buckets[index] += 1;
    }
    if count == 0 {
        println!("пауз между интервалами: 0");
        return;
    }
    println!(
        "пауз между интервалами: {count}, средняя {:.0} мс",
        total / count as f32
    );
    println!(
        "  <200 мс: {}, 200–500: {}, 500–1000: {}, 1000–1500: {}, 1500–3000: {}, >3000: {}",
        buckets[0], buckets[1], buckets[2], buckets[3], buckets[4], buckets[5]
    );
}

/// Склеивает интервалы так же, как `speech_ranges` в приложении.
fn merge(raw: &[(usize, usize)], gap_samples: usize) -> Vec<(usize, usize)> {
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in raw {
        if let Some((first, last_end)) = merged.last_mut()
            && start <= *last_end + gap_samples
            && end.saturating_sub(*first) <= 28 * 16_000
        {
            *last_end = (*last_end).max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn main() {
    let wav = std::env::args().nth(1).expect("wav");
    let model = std::env::args().nth(2).expect("vad model");
    let samples = load_wav(Path::new(&wav));
    println!("аудио: {:.1} с", samples.len() as f32 / 16_000.0);

    let mut context =
        WhisperVadContext::new(&model, WhisperVadContextParams::default()).expect("VAD");
    let mut params = WhisperVadParams::default();
    params.set_threshold(0.65);
    params.set_min_speech_duration(400);
    params.set_min_silence_duration(500);
    params.set_speech_pad(300);
    params.set_max_speech_duration(28.0);
    let detected = context
        .segments_from_samples(params, &samples)
        .expect("VAD segments");

    let mut raw_cs: Vec<(f32, f32)> = Vec::new();
    let mut raw: Vec<(usize, usize)> = Vec::new();
    for segment in detected {
        if segment.end > segment.start + 0.1 {
            raw_cs.push((segment.start, segment.end));
            let start = (segment.start * 160.0) as usize;
            let end = (segment.end * 160.0) as usize;
            raw.push((start.min(samples.len()), end.min(samples.len())));
        }
    }
    let speech: f32 = raw_cs.iter().map(|(s, e)| e - s).sum::<f32>() / 100.0;
    println!(
        "Silero: {} интервалов, речи {:.1} с ({:.1}%)",
        raw.len(),
        speech,
        speech / (samples.len() as f32 / 16_000.0) * 100.0
    );
    print_gaps(&raw_cs);

    for gap_ms in [300, 500, 1000, 1500] {
        let merged = merge(&raw, gap_ms * 16);
        let durations: Vec<f32> = merged
            .iter()
            .map(|(s, e)| (e - s) as f32 / 16_000.0)
            .collect();
        let longest = durations.iter().fold(0.0f32, |acc, value| acc.max(*value));
        println!(
            "склейка пауз ≤{gap_ms} мс: {} интервалов, самый длинный {:.1} с",
            merged.len(),
            longest
        );
    }
}
