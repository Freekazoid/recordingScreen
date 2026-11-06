//! Точка входа ML-воркера: построчный JSON-RPC через stdin/stdout.

use anyhow::Context;
use screenrecorder_ml_worker::run_stdio;

/// Запускает JSON-RPC-цикл.
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("screenrecorder-ml {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    run_stdio().context("ML-воркер")
}
