//! Запись системного звука и микрофона на Windows через WASAPI.
//!
//! Системный звук пишется в режиме loopback с устройства воспроизведения,
//! микрофон — с устройства записи. Дорожки раздельные, как на Linux:
//! `system.wav` и `mic.wav`. Формат фиксирован: PCM s16, 48 кГц, stereo.

use super::linux::{
    AudioDevice, AudioDeviceKind, AudioError, AudioProbe, AudioStart, analyze_track,
};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use wasapi::{
    Device, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta,
};

/// Частота дискретизации всех дорожек.
const SAMPLE_RATE: u32 = 48_000;
/// Число каналов: stereo для системного звука и микрофона.
const CHANNELS: u16 = 2;
/// Разрядность сэмпла.
const BITS: u16 = 16;

/// Одна записываемая дорожка.
#[derive(Debug)]
struct Track {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), String>>>,
}

/// Рекордер системного звука и микрофона.
#[derive(Debug, Default)]
pub struct WindowsAudioRecorder {
    system: Option<Track>,
    mic: Option<Track>,
}

impl WindowsAudioRecorder {
    /// Создаёт рекордер без запущенных потоков.
    pub fn new() -> Self {
        Self::default()
    }

    /// Возвращает активные устройства вывода и записи.
    ///
    /// Идентификатор — WASAPI endpoint id, он же сохраняется в настройках.
    pub fn list_devices() -> Result<Vec<AudioDevice>, AudioError> {
        initialize_mta()
            .ok()
            .map_err(|error| AudioError::Probe(format!("COM: {error}")))?;
        let enumerator = DeviceEnumerator::new()
            .map_err(|error| AudioError::Probe(format!("WASAPI: {error}")))?;
        let mut devices = Vec::new();
        for (direction, kind) in [
            (Direction::Render, AudioDeviceKind::System),
            (Direction::Capture, AudioDeviceKind::Mic),
        ] {
            let collection = enumerator
                .get_device_collection(&direction)
                .map_err(|error| AudioError::Probe(format!("WASAPI: {error}")))?;
            for device in &collection {
                let Ok(device) = device else {
                    continue;
                };
                let Ok(id) = device.get_id() else {
                    continue;
                };
                let label = device.get_friendlyname().unwrap_or_else(|_| id.clone());
                devices.push(AudioDevice { id, label, kind });
            }
        }
        Ok(devices)
    }

    /// Запускает `system.wav` и `mic.wav` в каталоге сессии.
    pub fn start(
        &mut self,
        session_dir: &Path,
        system_target: Option<&str>,
        mic_target: Option<&str>,
    ) -> Result<AudioStart, AudioError> {
        if self.system.is_some() || self.mic.is_some() {
            return Err(AudioError::AlreadyRunning);
        }
        std::fs::create_dir_all(session_dir).map_err(|source| AudioError::CreateDir {
            path: session_dir.to_path_buf(),
            source,
        })?;
        let mut warnings = Vec::new();
        self.system = start_track(&session_dir.join("system.wav"), true, system_target)
            .inspect_err(|error| warnings.push(format!("системный звук: {error}")))
            .ok();
        self.mic = start_track(&session_dir.join("mic.wav"), false, mic_target)
            .inspect_err(|error| warnings.push(format!("микрофон: {error}")))
            .ok();
        if self.system.is_none() && self.mic.is_none() {
            return Err(AudioError::NoDevices);
        }
        Ok(AudioStart {
            system: self.system.is_some(),
            mic: self.mic.is_some(),
            warnings,
        })
    }

    /// Останавливает дорожки и дописывает корректные WAV-заголовки.
    pub fn stop(&mut self) -> Result<(), AudioError> {
        stop_track(&mut self.system)?;
        stop_track(&mut self.mic)
    }

    /// Проверяет, живы ли потоки записи.
    pub fn is_running(&mut self) -> bool {
        track_running(&mut self.system) || track_running(&mut self.mic)
    }

    /// Записывает 1,2 секунды с выбранного устройства и считает RMS/peak.
    pub fn probe(target: Option<&str>, kind: AudioDeviceKind) -> Result<AudioProbe, AudioError> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "screenrecorder-probe-{}-{stamp}.wav",
            std::process::id()
        ));
        let track = start_track(&path, kind == AudioDeviceKind::System, target)
            .map_err(AudioError::Probe)?;
        thread::sleep(Duration::from_millis(1200));
        let mut track = Some(track);
        let _ = stop_track(&mut track);
        let analysis = analyze_track(&path);
        let _ = std::fs::remove_file(&path);
        let analysis = analysis?;
        Ok(AudioProbe {
            rms_dbfs: analysis.stats.rms_dbfs,
            peak_dbfs: analysis.stats.peak_dbfs,
            silent: analysis.stats.rms_dbfs < -50.0,
        })
    }
}

/// Запускает дорожку и ждёт подтверждения, что поток захвата стартовал.
fn start_track(path: &Path, system: bool, target: Option<&str>) -> Result<Track, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let path = path.to_path_buf();
    let target = target.map(str::to_string);
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name(
            if system {
                "wasapi-system"
            } else {
                "wasapi-mic"
            }
            .to_string(),
        )
        .spawn(move || capture_loop(&path, system, target.as_deref(), &worker_stop, &ready_tx))
        .map_err(|error| format!("поток WASAPI: {error}"))?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(Track {
            stop,
            handle: Some(handle),
        }),
        Ok(Err(error)) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            Err(error)
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
            Err("WASAPI не ответил за 5 секунд".to_string())
        }
    }
}

/// Останавливает дорожку и дожидается записи WAV-заголовка.
fn stop_track(track: &mut Option<Track>) -> Result<(), AudioError> {
    let Some(mut track) = track.take() else {
        return Ok(());
    };
    track.stop.store(true, Ordering::SeqCst);
    let Some(handle) = track.handle.take() else {
        return Ok(());
    };
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            tracing::warn!(%error, "ошибка WASAPI при остановке");
            Err(AudioError::Probe(error))
        }
        Err(_) => Err(AudioError::Probe("поток WASAPI паниковал".to_string())),
    }
}

/// Проверяет, что поток дорожки ещё работает.
fn track_running(track: &mut Option<Track>) -> bool {
    let Some(current) = track.as_mut() else {
        return false;
    };
    let Some(handle) = current.handle.as_ref() else {
        return false;
    };
    if handle.is_finished() {
        current.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = current.handle.take() {
            let _ = handle.join();
        }
        *track = None;
        false
    } else {
        true
    }
}

/// Находит устройство по endpoint id или берёт устройство по умолчанию.
fn select_device(
    enumerator: &DeviceEnumerator,
    direction: &Direction,
    target: Option<&str>,
) -> Result<Device, String> {
    if let Some(target) = target.filter(|value| !value.trim().is_empty()) {
        let collection = enumerator
            .get_device_collection(direction)
            .map_err(|error| format!("WASAPI: {error}"))?;
        for device in &collection {
            let Ok(device) = device else {
                continue;
            };
            if device.get_id().ok().as_deref() == Some(target) {
                return Ok(device);
            }
        }
    }
    enumerator
        .get_default_device(direction)
        .map_err(|error| format!("устройство по умолчанию: {error}"))
}

/// Поток захвата одной дорожки.
fn capture_loop(
    path: &Path,
    system: bool,
    target: Option<&str>,
    stop: &AtomicBool,
    ready: &Sender<Result<(), String>>,
) -> Result<(), String> {
    let result = capture_inner(path, system, target, stop, ready);
    if let Err(error) = &result {
        let _ = ready.send(Err(error.clone()));
        tracing::warn!(%error, system, "захват WASAPI завершился ошибкой");
    }
    result
}

/// Открывает WASAPI-поток и пишет сэмплы в WAV до флага остановки.
fn capture_inner(
    path: &Path,
    system: bool,
    target: Option<&str>,
    stop: &AtomicBool,
    ready: &Sender<Result<(), String>>,
) -> Result<(), String> {
    initialize_mta()
        .ok()
        .map_err(|error| format!("COM: {error}"))?;
    let enumerator = DeviceEnumerator::new().map_err(|error| format!("WASAPI: {error}"))?;
    // Для loopback устройство — рендер, а поток открывается как capture.
    let direction = if system {
        Direction::Render
    } else {
        Direction::Capture
    };
    let device = select_device(&enumerator, &direction, target)?;
    let mut client = device
        .get_iaudioclient()
        .map_err(|error| format!("WASAPI: {error}"))?;
    let format = WaveFormat::new(
        usize::from(BITS),
        usize::from(BITS),
        &SampleType::Int,
        SAMPLE_RATE as usize,
        CHANNELS as usize,
        None,
    );
    let (_, min_period) = client
        .get_device_period()
        .map_err(|error| format!("период WASAPI: {error}"))?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: min_period,
    };
    client
        .initialize_client(&format, &Direction::Capture, &mode)
        .map_err(|error| format!("инициализация WASAPI: {error}"))?;
    let event = client
        .set_get_eventhandle()
        .map_err(|error| format!("событие WASAPI: {error}"))?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|error| format!("клиент захвата: {error}"))?;
    let mut sink = WavSink::create(path)?;
    client
        .start_stream()
        .map_err(|error| format!("старт WASAPI: {error}"))?;
    let _ = ready.send(Ok(()));

    let mut queue: VecDeque<u8> = VecDeque::new();
    let mut bytes: Vec<u8> = Vec::new();
    let result = loop {
        if stop.load(Ordering::SeqCst) {
            break Ok(());
        }
        if let Err(error) = capture.read_from_device_to_deque(&mut queue) {
            break Err(format!("чтение WASAPI: {error}"));
        }
        if !queue.is_empty() {
            bytes.clear();
            bytes.extend(queue.drain(..));
            if let Err(error) = sink.write(&bytes) {
                break Err(format!("запись WAV: {error}"));
            }
        }
        // Таймаут ограничивает реакцию на остановку сотней миллисекунд.
        let _ = event.wait_for_event(100);
        if let Err(error) = sink.fill_silence() {
            break Err(format!("запись тишины: {error}"));
        }
    };
    let _ = client.stop_stream();
    if let Err(error) = sink.finalize() {
        if result.is_ok() {
            return Err(format!("заголовок WAV: {error}"));
        }
    }
    result
}

/// WAV-файл с потоковой записью и обновлением заголовка при остановке.
struct WavSink {
    file: File,
    data_bytes: u64,
    started: Instant,
    bytes_per_second: u64,
}

impl WavSink {
    /// Создаёт файл с временным заголовком.
    fn create(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("каталог {}: {error}", parent.display()))?;
        }
        let mut file =
            File::create(path).map_err(|error| format!("создание {}: {error}", path.display()))?;
        write_header(&mut file, 0, CHANNELS, SAMPLE_RATE).map_err(|error| error.to_string())?;
        Ok(Self {
            file,
            data_bytes: 0,
            started: Instant::now(),
            bytes_per_second: u64::from(SAMPLE_RATE) * u64::from(CHANNELS) * u64::from(BITS / 8),
        })
    }

    /// Дописывает блок PCM-сэмплов.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.file.write_all(bytes)?;
        self.data_bytes += bytes.len() as u64;
        Ok(())
    }

    /// Дописывает тишину, если WASAPI не прислал пакеты.
    ///
    /// Loopback-устройство молчит, когда в системе ничего не играет, а
    /// виртуальный микрофон может перестать отдавать данные. Без явной
    /// тишины дорожка обрывалась бы и рассинхронизировалась с видео.
    fn fill_silence(&mut self) -> std::io::Result<()> {
        let expected =
            self.started.elapsed().as_micros() as u64 * self.bytes_per_second / 1_000_000;
        let mut missing = expected.saturating_sub(self.data_bytes);
        while missing > 4096 {
            // Пишем не больше секунды за раз, чтобы не держать большой буфер.
            let chunk = missing.min(self.bytes_per_second) as usize;
            let zeros = vec![0_u8; chunk - chunk % 4];
            self.write(&zeros)?;
            missing -= zeros.len() as u64;
        }
        Ok(())
    }

    /// Возвращает реальный размер данных в заголовок.
    fn finalize(mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.seek(SeekFrom::Start(0))?;
        write_header(&mut self.file, self.data_bytes, CHANNELS, SAMPLE_RATE)?;
        self.file.flush()
    }
}

/// Пишет 44-байтовый заголовок PCM s16.
fn write_header(
    file: &mut File,
    data_bytes: u64,
    channels: u16,
    sample_rate: u32,
) -> std::io::Result<()> {
    let data_size = data_bytes.min(u32::MAX as u64 - 36) as u32;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(BITS / 8);
    let block_align = channels * (BITS / 8);
    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_size).to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?;
    file.write_all(&1_u16.to_le_bytes())?;
    file.write_all(&channels.to_le_bytes())?;
    file.write_all(&sample_rate.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&BITS.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    Ok(())
}
