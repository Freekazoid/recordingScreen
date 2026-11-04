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

/// Запущенный ML-воркер.
pub struct MlWorker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl MlWorker {
    /// Запускает `screenrecorder-ml` и подключается к его stdin/stdout.
    pub fn spawn(models_dir: &Path) -> Result<Self, String> {
        let binary = find_worker_binary()?;
        let mut child = Command::new(&binary)
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

/// Ищет бинарник ML-воркера рядом с приложением.
fn find_worker_binary() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("SCREENRECORDER_ML_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
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
            return Ok(candidate);
        }
    }
    Err("ML-воркер не найден рядом с приложением; соберите screenrecorder-ml".to_string())
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
        assert_eq!(found, path);
        unsafe {
            std::env::remove_var("SCREENRECORDER_ML_BIN");
        }
        let _ = std::fs::remove_file(path);
    }
}
