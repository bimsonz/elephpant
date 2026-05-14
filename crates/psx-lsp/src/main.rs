//! `psx-lsp` binary — thin shim around the LSP backend. Reads/writes the
//! LSP protocol over stdin/stdout so editors can spawn it directly.

use psx_lsp::Backend;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
