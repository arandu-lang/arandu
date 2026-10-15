use lsp_server::Connection;
use std::error::Error;

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    // The VS Code extension validates the discovered server with --version
    // before starting the client, matching the rust-analyzer contract.
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!("arandu-lsp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let (connection, io_threads) = Connection::stdio();
    let result = arandu_lsp::run(connection);
    // Drain protocol output even when initialization fails. Returning early
    // can terminate the writer before it flushes the InvalidParams response.
    let io_result = io_threads.join();
    result?;
    io_result?;
    Ok(())
}
