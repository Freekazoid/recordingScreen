//! Точка входа ML-воркера: построчный JSON-RPC через stdin/stdout.

use anyhow::Context;
use screenrecorder_ml_worker::Server;
use std::io::{self, BufRead, Write};

/// Читает запросы из stdin и пишет ответы в stdout.
fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("screenrecorder-ml {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

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
