//! Language Server Protocol implementation for PHPScript.
//!
//! Backend on `tower-lsp`. Documents are parsed on `did_open` / `did_change`
//! and held in a `DashMap`-backed index keyed by URL. Diagnostics fall out
//! of the parse error; hover, goto-definition, and the symbol providers
//! walk the cached AST. The trait map + resolver from `psx-cli` give us
//! cross-file `use`-jumping for free.

pub mod index;
pub mod server;

pub use server::Backend;
