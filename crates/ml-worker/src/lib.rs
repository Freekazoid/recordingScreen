//! JSON-RPC-сервер ML-воркера.
//!
//! ASR работает через whisper-rs; диаризация пока отвечает понятной
//! ошибкой «модель не подключена».

use screenrecorder_ml::WhisperEngine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Версия протокола Rust core ↔ ML worker.
pub const PROTOCOL_VERSION: u32 = 1;

/// Входящий JSON-RPC-запрос.
#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// Параметры `asr.transcribe`.
#[derive(Debug, Deserialize)]
struct AsrParams {
    audio_path: PathBuf,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    timestamps: Option<bool>,
}

/// Исходящий JSON-RPC-ответ.
#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

/// Ошибка JSON-RPC.
#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl RpcResponse {
    /// Создаёт успешный ответ.
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Создаёт ответ с ошибкой.
    fn err(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Состояние ML-воркера.
pub struct Server {
    running: bool,
    models_dir: PathBuf,
    asr: Option<WhisperEngine>,
}

impl Server {
    /// Создаёт сервер в рабочем состоянии.
    pub fn new() -> Self {
        let models_dir = std::env::var_os("SCREENRECORDER_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("models"));
        Self::with_models_dir(models_dir)
    }

    /// Создаёт сервер с явным каталогом моделей.
    pub fn with_models_dir(models_dir: impl Into<PathBuf>) -> Self {
        Self {
            running: true,
            models_dir: models_dir.into(),
            asr: None,
        }
    }

    /// Возвращает `true`, пока воркер должен обрабатывать запросы.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Путь к модели Whisper.
    fn whisper_path(&self) -> PathBuf {
        self.models_dir
            .join("whisper")
            .join("ggml-large-v3-turbo-q5_0.bin")
    }

    /// Проверяет готовность моделей.
    fn model_status(&self) -> Value {
        json!({
            "whisper": self.whisper_path().is_file(),
            "diarization": self.models_dir.join("diarization/segmentation.onnx").is_file()
                && self.models_dir.join("diarization/embedding.onnx").is_file(),
            "vad": self.models_dir.join("vad/silero_vad.onnx").is_file(),
        })
    }

    /// Выполняет `asr.transcribe`.
    fn transcribe(&mut self, id: Value, params: Option<Value>) -> RpcResponse {
        let params: AsrParams = match serde_json::from_value(params.unwrap_or_default()) {
            Ok(params) => params,
            Err(error) => {
                return RpcResponse::err(id, -32602, format!("параметры ASR: {error}"));
            }
        };
        if !params.audio_path.is_file() {
            return RpcResponse::err(
                id,
                -32002,
                format!("аудиофайл не найден: {}", params.audio_path.display()),
            );
        }
        let model = self.whisper_path();
        if !model.is_file() {
            return RpcResponse::err(
                id,
                -32001,
                format!("модель ASR не найдена: {}", model.display()),
            );
        }
        let engine = self.asr.get_or_insert_with(|| WhisperEngine::new(&model));
        match engine.transcribe(
            &params.audio_path,
            params.language.as_deref(),
            params.timestamps.unwrap_or(true),
        ) {
            Ok(transcript) => match serde_json::to_value(transcript) {
                Ok(value) => RpcResponse::ok(id, value),
                Err(error) => RpcResponse::err(id, -32603, format!("сериализация: {error}")),
            },
            Err(error) => RpcResponse::err(id, -32002, error.to_string()),
        }
    }

    /// Обрабатывает одну строку JSON и возвращает строку ответа.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let request: RpcRequest = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                let response =
                    RpcResponse::err(Value::Null, -32700, format!("ошибка разбора JSON: {error}"));
                return Some(self.encode(response));
            }
        };

        if request.jsonrpc != "2.0" {
            let response = RpcResponse::err(
                request.id.unwrap_or(Value::Null),
                -32600,
                "ожидается JSON-RPC 2.0",
            );
            return Some(self.encode(response));
        }

        let id = request.id.unwrap_or(Value::Null);
        let params = request.params;
        let method = request.method;
        let status = self.model_status();
        let response = match method.as_str() {
            "hello" => RpcResponse::ok(
                id,
                json!({
                    "protocol": PROTOCOL_VERSION,
                    "name": "screenrecorder-ml",
                    "version": env!("CARGO_PKG_VERSION"),
                    "engines": {
                        "asr": "whisper-rs",
                        "diarization": "sherpa-onnx"
                    },
                    "models": status
                }),
            ),
            "models.status" => RpcResponse::ok(id, status),
            "asr.transcribe" => self.transcribe(id, params),
            "diarize.run" => {
                RpcResponse::err(id, -32001, "модель диаризации ещё не подключена (этап 4)")
            }
            "speaker.embed" => {
                RpcResponse::err(id, -32001, "эмбеддинги спикеров ещё не подключены (этап 4)")
            }
            "cancel" => RpcResponse::ok(id, Value::Null),
            "shutdown" => {
                self.running = false;
                RpcResponse::ok(id, Value::Null)
            }
            other => RpcResponse::err(id, -32601, format!("неизвестный метод: {other}")),
        };

        Some(self.encode(response))
    }

    /// Сериализует ответ; при сбое возвращает минимальный JSON-RPC error.
    fn encode(&self, response: RpcResponse) -> String {
        serde_json::to_string(&response).unwrap_or_else(|_| {
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal error"}}"#
                .to_string()
        })
    }
}

impl Default for Server {
    /// Создаёт сервер по умолчанию.
    fn default() -> Self {
        Self::new()
    }
}

/// Читает JSON-RPC-запросы из stdin и пишет ответы в stdout.
pub fn run_stdio() -> anyhow::Result<()> {
    use anyhow::Context;
    use std::io::{self, BufRead, Write};

    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout());
    let mut server = Server::new();
    for line in stdin.lock().lines() {
        let line = line.context("не удалось прочитать строку stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle_line(&line) {
            writeln!(stdout, "{response}").context("не удалось записать ответ")?;
            stdout.flush().context("не удалось сбросить stdout")?;
        }
        if !server.is_running() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет handshake и версию протокола.
    #[test]
    fn hello_returns_protocol() {
        let mut server = Server::new();
        let response = server
            .handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"hello"}"#)
            .expect("ответ");
        let value: Value = serde_json::from_str(&response).expect("json");
        assert_eq!(value["result"]["protocol"], PROTOCOL_VERSION);
        assert_eq!(value["result"]["engines"]["asr"], "whisper-rs");
    }

    /// Проверяет ошибку на неизвестный метод.
    #[test]
    fn unknown_method_returns_error() {
        let mut server = Server::new();
        let response = server
            .handle_line(r#"{"jsonrpc":"2.0","id":2,"method":"unknown"}"#)
            .expect("ответ");
        let value: Value = serde_json::from_str(&response).expect("json");
        assert_eq!(value["error"]["code"], -32601);
    }

    /// Проверяет понятную ошибку при отсутствии модели.
    #[test]
    fn transcribe_reports_missing_model() {
        let dir = tempfile::tempdir().expect("tempdir");
        let audio = dir.path().join("audio.wav");
        std::fs::write(&audio, b"RIFF").expect("audio");
        let mut server = Server::with_models_dir(dir.path());
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "asr.transcribe",
            "params": {"audio_path": audio}
        });
        let response = server.handle_line(&request.to_string()).expect("ответ");
        let value: Value = serde_json::from_str(&response).expect("json");
        assert_eq!(value["error"]["code"], -32001);
    }

    /// Проверяет корректное завершение по `shutdown`.
    #[test]
    fn shutdown_stops_server() {
        let mut server = Server::new();
        let response = server
            .handle_line(r#"{"jsonrpc":"2.0","id":4,"method":"shutdown"}"#)
            .expect("ответ");
        assert!(response.contains("result"));
        assert!(!server.is_running());
    }
}
