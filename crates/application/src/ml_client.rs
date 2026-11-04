//! Клиент ML-воркера по JSON-RPC через stdin/stdout.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Сегмент распознанной речи.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
}

/// Результат распознавания.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub language: String,
    pub segments: Vec<TranscriptSegment>,
}

/// Реплика спикера.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerTurn {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: String,
}

/// Запущенный ML-воркер.
pub struct MlWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl MlWorker {
    /// Запускает ML-воркер и подключается к его stdin/stdout.
    pub fn spawn(models_dir: &Path) -> Result<Self, String> {
        let (binary, embedded) = find_worker_binary()?;
        let mut command = Command::new(&binary);
        if embedded {
            command.arg("--ml-worker");
        }
        let mut child = command
            .env("SCREENRECORDER_MODELS_DIR", models_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("ML-воркер {}: {error}", binary.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ML-воркер: нет stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "ML-воркер: нет stdout".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    /// Распознаёт аудиофайл.
    pub fn transcribe(
        &mut self,
        audio: &Path,
        language: Option<&str>,
        timestamps: bool,
    ) -> Result<Transcript, String> {
        let result = self.request(
            "asr.transcribe",
            serde_json::json!({
                "audio_path": audio,
                "language": language,
                "timestamps": timestamps,
            }),
        )?;
        serde_json::from_value(result).map_err(|error| format!("ответ ASR: {error}"))
    }

    /// Отправляет JSON-RPC-запрос и ждёт ответ.
    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{request}").map_err(|error| format!("ML stdin: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("ML flush: {error}"))?;
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .map_err(|error| format!("ML stdout: {error}"))?;
        if read == 0 {
            let status = self.child.try_wait().ok().flatten();
            return Err(format!("ML-воркер завершился: {status:?}"));
        }
        let response: serde_json::Value =
            serde_json::from_str(&line).map_err(|error| format!("ответ ML-воркера: {error}"))?;
        if let Some(error) = response.get("error") {
            return Err(error
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("ошибка ML-воркера")
                .to_string());
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| "ML-воркер вернул пустой ответ".to_string())
    }
}

impl Drop for MlWorker {
    /// Завершает воркер при выходе.
    fn drop(&mut self) {
        let _ = writeln!(
            self.stdin,
            r#"{{"jsonrpc":"2.0","id":0,"method":"shutdown"}}"#
        );
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Запущенный воркер диаризации (отдельный процесс, без whisper).
pub struct DiarizeWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl DiarizeWorker {
    /// Запускает `screenrecorder-diarize`.
    pub fn spawn(models_dir: &Path) -> Result<Self, String> {
        let binary = find_diarize_binary()?;
        let mut child = Command::new(&binary)
            .env("SCREENRECORDER_MODELS_DIR", models_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("воркер диаризации {}: {error}", binary.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "воркер диаризации: нет stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "воркер диаризации: нет stdout".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    /// Размечает спикеров.
    pub fn diarize(
        &mut self,
        audio: &Path,
        min_speakers: Option<u32>,
        max_speakers: Option<u32>,
    ) -> Result<Vec<SpeakerTurn>, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "diarize.run",
            "params": {
                "audio_path": audio,
                "min_speakers": min_speakers,
                "max_speakers": max_speakers,
            },
        });
        writeln!(self.stdin, "{request}").map_err(|error| format!("diarize stdin: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("diarize flush: {error}"))?;
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .map_err(|error| format!("diarize stdout: {error}"))?;
        if read == 0 {
            let status = self.child.try_wait().ok().flatten();
            return Err(format!("воркер диаризации завершился: {status:?}"));
        }
        let response: serde_json::Value =
            serde_json::from_str(&line).map_err(|error| format!("ответ диаризации: {error}"))?;
        if let Some(error) = response.get("error") {
            return Err(error
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("ошибка диаризации")
                .to_string());
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| "воркер диаризации вернул пустой ответ".to_string())?;
        serde_json::from_value(result).map_err(|error| format!("сегменты диаризации: {error}"))
    }
}

impl Drop for DiarizeWorker {
    /// Завершает воркер при выходе.
    fn drop(&mut self) {
        let _ = writeln!(
            self.stdin,
            r#"{{"jsonrpc":"2.0","id":0,"method":"shutdown"}}"#
        );
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Запущенный NeMo-воркер диаризации.
pub struct NemoWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl NemoWorker {
    /// Запускает `screenrecorder-nemo`.
    pub fn spawn(models_dir: &Path) -> Result<Self, String> {
        let binary = find_nemo_binary()?;
        let mut child = Command::new(&binary)
            .env("SCREENRECORDER_MODELS_DIR", models_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("NeMo-воркер {}: {error}", binary.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "NeMo-воркер: нет stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "NeMo-воркер: нет stdout".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    /// Размечает спикеров через NeMo Sortformer.
    pub fn diarize(&mut self, audio: &Path) -> Result<Vec<SpeakerTurn>, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "diarize.run",
            "params": {"audio_path": audio},
        });
        writeln!(self.stdin, "{request}").map_err(|error| format!("NeMo stdin: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("NeMo flush: {error}"))?;
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .map_err(|error| format!("NeMo stdout: {error}"))?;
        if read == 0 {
            let status = self.child.try_wait().ok().flatten();
            return Err(format!("NeMo-воркер завершился: {status:?}"));
        }
        let response: serde_json::Value =
            serde_json::from_str(&line).map_err(|error| format!("ответ NeMo: {error}"))?;
        if let Some(error) = response.get("error") {
            return Err(error
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("ошибка NeMo")
                .to_string());
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| "NeMo-воркер вернул пустой ответ".to_string())?;
        serde_json::from_value(result).map_err(|error| format!("сегменты NeMo: {error}"))
    }
}

impl Drop for NemoWorker {
    /// Завершает воркер при выходе.
    fn drop(&mut self) {
        let _ = writeln!(
            self.stdin,
            r#"{{"jsonrpc":"2.0","id":0,"method":"shutdown"}}"#
        );
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Ищет бинарник NeMo-воркера рядом с приложением.
fn find_nemo_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("SCREENRECORDER_NEMO_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "SCREENRECORDER_NEMO_BIN указывает на несуществующий файл: {}",
            path.display()
        ));
    }
    let exe = std::env::current_exe().map_err(|error| format!("current_exe: {error}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "не удалось определить каталог приложения".to_string())?;
    for name in ["screenrecorder-nemo", "screenrecorder-nemo.exe"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("NeMo-воркер не найден; соберите screenrecorder-nemo".to_string())
}

/// Ищет бинарник воркера диаризации рядом с приложением.
fn find_diarize_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("SCREENRECORDER_DIARIZE_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "SCREENRECORDER_DIARIZE_BIN указывает на несуществующий файл: {}",
            path.display()
        ));
    }
    let exe = std::env::current_exe().map_err(|error| format!("current_exe: {error}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "не удалось определить каталог приложения".to_string())?;
    for name in ["screenrecorder-diarize", "screenrecorder-diarize.exe"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err("воркер диаризации не найден; соберите screenrecorder-diarize".to_string())
}

/// Ищет бинарник ML-воркера рядом с приложением.
///
/// Возвращает путь и признак `--ml-worker`: если отдельного бинарника нет,
/// используется сам исполняемый файл приложения с этим режимом.
fn find_worker_binary() -> Result<(PathBuf, bool), String> {
    if let Some(path) = std::env::var_os("SCREENRECORDER_ML_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok((path, false));
        }
        return Err(format!(
            "SCREENRECORDER_ML_BIN указывает на несуществующий файл: {}",
            path.display()
        ));
    }
    let exe = std::env::current_exe().map_err(|error| format!("current_exe: {error}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "не удалось определить каталог приложения".to_string())?;
    for name in ["screenrecorder-ml", "screenrecorder-ml.exe"] {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok((candidate, false));
        }
    }
    Ok((exe, true))
}

/// Назначает спикеров сегментам по максимальному пересечению.
pub fn assign_speakers(transcript: &mut Transcript, turns: &[SpeakerTurn]) {
    for segment in &mut transcript.segments {
        let mut best: Option<(u64, &str)> = None;
        for turn in turns {
            let start = segment.start_ms.max(turn.start_ms);
            let end = segment.end_ms.min(turn.end_ms);
            if end <= start {
                continue;
            }
            let overlap = end - start;
            if best.is_none_or(|(current, _)| overlap > current) {
                best = Some((overlap, turn.speaker.as_str()));
            }
        }
        if let Some((_, speaker)) = best {
            segment.speaker = Some(speaker.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет поиск бинарника с явной переменной окружения.
    #[test]
    fn finds_binary_from_env() {
        let path = std::env::temp_dir().join(format!("ml-worker-{}", std::process::id()));
        std::fs::write(&path, b"test").expect("write");
        // SAFETY: тест однопоточный по умолчанию, переменная только для этого теста.
        unsafe {
            std::env::set_var("SCREENRECORDER_ML_BIN", &path);
        }
        let found = find_worker_binary().expect("binary");
        assert_eq!(found, (path.clone(), false));
        unsafe {
            std::env::remove_var("SCREENRECORDER_ML_BIN");
        }
        let _ = std::fs::remove_file(path);
    }
}
