use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use psx_cli::{compile_project_with, compile_str, CompileOptions};
use psx_resolver::PsxConfig;

#[derive(Debug, Parser)]
#[command(
    name = "psx",
    about = "PHPScript (.psx) compiler — PHP-flavored TypeScript",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compile .psx sources to .ts.
    ///
    /// - `psx build <file.psx>`: single-file. Output to <source-dir>/dist/<stem>.ts.
    /// - `psx build <directory>`: walks the directory; compiles every .psx
    ///   it finds. Output preserves structure under <directory>/dist.
    /// - `psx build` (no path): looks for `psx.json` in the current
    ///   directory and walks up. Compiles the project per its config.
    Build {
        /// Source path (file or directory). Optional — when omitted, builds
        /// the project rooted at the closest `psx.json`.
        path: Option<PathBuf>,
        /// Skip writing `.ts.map` source-map files and the trailing
        /// `sourceMappingURL` comment. Source maps are on by default.
        #[arg(long)]
        no_source_maps: bool,
    },
    /// Type-check .psx sources without emitting (Phase 5+).
    Check { path: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            println!(
                "psx {} — run `psx --help` for commands.",
                env!("CARGO_PKG_VERSION")
            );
        }
        Some(Command::Build {
            path,
            no_source_maps,
        }) => {
            let options = CompileOptions {
                source_maps: !no_source_maps,
            };
            run_build(path, &options)?;
        }
        Some(Command::Check { path }) => {
            anyhow::bail!(
                "`psx check {}` is not implemented yet (lands in Phase 5).",
                path.display()
            );
        }
    }

    Ok(())
}

/// Dispatch `psx build [<path>]` based on what `path` points to:
///
/// - `None`: discover `psx.json` from CWD upward, then compile project.
/// - `Some(path)` where `path` is a file: single-file mode (legacy).
/// - `Some(path)` where `path` is a directory: project mode rooted at that
///   directory (must contain `psx.json` directly OR an ancestor with one).
fn run_build(path: Option<PathBuf>, options: &CompileOptions) -> Result<()> {
    match path {
        None => build_project_from_cwd(options),
        Some(p) if p.is_file() => build_single_file(&p),
        Some(p) if p.is_dir() => build_project_from(&p, options),
        Some(p) => Err(anyhow::anyhow!(
            "{} is neither a file nor a directory",
            p.display()
        )),
    }
}

fn build_single_file(path: &std::path::Path) -> Result<()> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let ts =
        compile_str(&source).with_context(|| format!("failed to compile {}", path.display()))?;
    let stem = path
        .file_stem()
        .ok_or_else(|| anyhow::anyhow!("{} has no file stem", path.display()))?;
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let dist_dir = parent.join("dist");
    fs::create_dir_all(&dist_dir)
        .with_context(|| format!("failed to create {}", dist_dir.display()))?;
    let out_path = dist_dir.join(stem).with_extension("ts");
    fs::write(&out_path, &ts).with_context(|| format!("failed to write {}", out_path.display()))?;
    eprintln!("wrote {}", out_path.display());
    Ok(())
}

fn build_project_from_cwd(options: &CompileOptions) -> Result<()> {
    let cwd = std::env::current_dir().context("cannot read current working directory")?;
    let (root, config) = PsxConfig::discover_from(&cwd).ok_or_else(|| {
        anyhow::anyhow!("no `psx.json` found in {} or any ancestor", cwd.display())
    })?;
    build_project_at(&config, &root, options)
}

fn build_project_from(start: &std::path::Path, options: &CompileOptions) -> Result<()> {
    let (root, config) = PsxConfig::discover_from(start).ok_or_else(|| {
        anyhow::anyhow!("no `psx.json` found in {} or any ancestor", start.display())
    })?;
    build_project_at(&config, &root, options)
}

fn build_project_at(
    config: &PsxConfig,
    root: &std::path::Path,
    options: &CompileOptions,
) -> Result<()> {
    let written = compile_project_with(config, root, options)?;
    for (src, dest) in &written {
        eprintln!("wrote {} -> {}", src.display(), dest.display());
    }
    if written.is_empty() {
        eprintln!(
            "no .psx files found under {}",
            root.join(&config.src).display()
        );
    }
    Ok(())
}
