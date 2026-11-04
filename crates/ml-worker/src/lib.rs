//! JSON-RPC-сервер ML-воркера.
//!
//! Пока это каркас: ASR и диаризация отвечают понятной ошибкой
//! «модель не подключена», а `hello` и `models.status` уже готовы для
//! интеграции с Rust-ядром.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Версия протокола Rust core ↔ ML worker.
pub const PROTOCOL_VERSION: u32 = 1;

/// Входящий JSON-RPC-запрос.
#[derive(Debug, Deserialize)]
struct RpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
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
}

impl Server {
    /// Создаёт сервер в рабочем состоянии.
    pub fn new() -> Self {
        Self { running: true }
    }

    /// Возвращает `true`, пока воркер должен обрабатывать запросы.
    pub fn is_running(&self) -> bool {
        self.running
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
        let response = match request.method.as_str() {
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
                    "models": {
                        "whisper": false,
                        "diarization": false
                    }
                }),
            ),
            "models.status" => RpcResponse::ok(
                id,
                json!({
                    "whisper": false,
                    "diarization": false,
                    "vad": false
                }),
            ),
            "asr.transcribe" => {
                RpcResponse::err(id, -32001, "модель ASR ещё не подключена (этап 4)")
            }
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

    /// Проверяет понятную ошибку до подключения модели.
    #[test]
    fn transcribe_reports_missing_model() {
        let mut server = Server::new();
        let response = server
            .handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"asr.transcribe","params":{}}"#)
            .expect("ответ");
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
