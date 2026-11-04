//! JSON-RPC-воркер NeMo Sortformer (отдельный процесс).

use anyhow::Context;
use screenrecorder_nemo::NemoDiarizer;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

/// Параметры `diarize.run`.
#[derive(Debug, Deserialize)]
struct DiarizeParams {
    audio_path: PathBuf,
}

/// Состояние воркера.
struct Server {
    running: bool,
    model_path: PathBuf,
    engine: Option<NemoDiarizer>,
}

impl Server {
    /// Создаёт воркер.
    fn new() -> Self {
        let models_dir = std::env::var_os("SCREENRECORDER_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("models"));
        Self {
            running: true,
            model_path: models_dir.join("nemo/nemotron3_diar_v3.onnx"),
            engine: None,
        }
    }

    /// Обрабатывает один запрос.
    fn handle(&mut self, request: &Value) -> Option<String> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let response = match method {
            "hello" => success(id, json!({"protocol": 1, "name": "screenrecorder-nemo"})),
            "models.status" => success(id, json!({"nemo": self.model_path.is_file()})),
            "diarize.run" => {
                let params = request.get("params").cloned().unwrap_or_default();
                let params: DiarizeParams = match serde_json::from_value(params) {
                    Ok(params) => params,
                    Err(error) => {
                        return Some(rpc_error(id, -32602, format!("параметры: {error}")));
                    }
                };
                if !params.audio_path.is_file() {
                    return Some(rpc_error(
                        id,
                        -32002,
                        format!("аудиофайл не найден: {}", params.audio_path.display()),
                    ));
                }
                if !self.model_path.is_file() {
                    return Some(rpc_error(id, -32001, "модель NeMo не найдена"));
                }
                let engine = self
                    .engine
                    .get_or_insert_with(|| NemoDiarizer::new(&self.model_path));
                match engine.diarize(&params.audio_path) {
                    Ok(turns) => match serde_json::to_value(turns) {
                        Ok(value) => success(id, value),
                        Err(error) => rpc_error(id, -32603, format!("сериализация: {error}")),
                    },
                    Err(error) => rpc_error(id, -32002, error.to_string()),
                }
            }
            "shutdown" => {
                self.running = false;
                success(id, Value::Null)
            }
            other => rpc_error(id, -32601, format!("неизвестный метод: {other}")),
        };
        Some(response)
    }
}

/// Успешный JSON-RPC-ответ.
fn success(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// JSON-RPC-ответ с ошибкой.
fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> String {
    let message = message.into();
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Запускает цикл stdin/stdout.
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("screenrecorder-nemo {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout());
    let mut server = Server::new();
    for line in stdin.lock().lines() {
        let line = line.context("stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                writeln!(
                    stdout,
                    "{}",
                    rpc_error(Value::Null, -32700, format!("JSON: {error}"))
                )?;
                stdout.flush()?;
                continue;
            }
        };
        if let Some(response) = server.handle(&request) {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
        if !server.running {
            break;
        }
    }
    Ok(())
}
