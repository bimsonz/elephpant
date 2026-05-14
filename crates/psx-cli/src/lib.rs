//! Programmatic entry points for the `psx` CLI. Tests and host integrations
//! call into this crate so the binary stays a thin shell over the library.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use psx_ast::{Module, Stmt, TraitDecl};
use psx_emitter::{
    emit, emit_with_resolver_and_traits, emit_with_source_map, SourceMapInput, TraitMap,
};
use psx_parser::{parse, ParseError};
use psx_resolver::{resolve_use, PsxConfig, ResolvedImport};

/// Knobs that affect `compile_project` output beyond the project's
/// `psx.json` (which controls input/output layout). Defaults to "everything
/// on, including source maps".
#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// Emit a `.ts.map` source-map file next to each emitted `.ts` and add
    /// a trailing `//# sourceMappingURL` comment.
    pub source_maps: bool,
}

impl Default for CompileOptions {
    fn default() -> Self {
        Self { source_maps: true }
    }
}

/// Compile a single `.psx` source string into a TypeScript source string.
/// Single-file mode — no resolver context, so `use` paths are emitted
/// verbatim (joined with `/`).
pub fn compile_str(source: &str) -> Result<String, ParseError> {
    let module = parse(source)?;
    Ok(emit(&module))
}

/// Read a single `.psx` file, parse it, and emit TS as a single-file (no
/// project resolver).
pub fn compile_file(path: &Path) -> Result<String> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    compile_str(&source).with_context(|| format!("failed to compile {}", path.display()))
}

/// Compile every `.psx` file under `<project_root>/<config.src>` to its
/// corresponding `.ts` file under `<project_root>/<config.out_dir>`,
/// preserving directory structure.
///
/// Two passes:
/// 1. Parse every `.psx` file into a `Module`. Collect every `Stmt::Trait`
///    declaration into a project-wide map keyed by trait name.
/// 2. For each parsed Module, emit TS using `emit_with_resolver_and_traits`
///    so cross-file `use TraitX;` inside class bodies inlines correctly.
///
/// Returns a list of (source, dest) pairs that were written.
pub fn compile_project(config: &PsxConfig, project_root: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    compile_project_with(config, project_root, &CompileOptions::default())
}

pub fn compile_project_with(
    config: &PsxConfig,
    project_root: &Path,
    options: &CompileOptions,
) -> Result<Vec<(PathBuf, PathBuf)>> {
    let src_root = project_root.join(&config.src);
    let out_root = project_root.join(&config.out_dir);
    if !src_root.is_dir() {
        anyhow::bail!("source directory does not exist: {}", src_root.display());
    }
    let psx_files = collect_psx_files(&src_root)?;

    // Pass 1: parse every file. Keep the source text alongside the parsed
    // Module so the emitter can build a LineMap for source-map output.
    let mut parsed: Vec<(PathBuf, String, Module)> = Vec::with_capacity(psx_files.len());
    for psx_path in psx_files {
        let source = std::fs::read_to_string(&psx_path)
            .with_context(|| format!("failed to read {}", psx_path.display()))?;
        let module =
            parse(&source).with_context(|| format!("failed to parse {}", psx_path.display()))?;
        parsed.push((psx_path, source, module));
    }

    // Build the project-wide trait map. Keyed by bare trait name; later
    // declarations overwrite earlier ones if duplicate names exist (the
    // user's problem to resolve).
    let mut traits: TraitMap<'_> = BTreeMap::new();
    for (_, _, module) in &parsed {
        for stmt in &module.stmts {
            if let Stmt::Trait(t) = stmt {
                traits.insert(t.name.clone(), t as &TraitDecl);
            }
        }
    }

    // Pass 2: emit each file with the trait map + use-resolver.
    let mut written = Vec::new();
    for (psx_path, source, module) in &parsed {
        let rel = psx_path
            .strip_prefix(&src_root)
            .expect("collected file lives under src_root");
        let out_path = out_root.join(rel).with_extension("ts");
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let project_root_buf = project_root.to_path_buf();
        let cfg = config.clone();
        let current = psx_path.clone();
        let resolver = move |path: &[String], alias: Option<&str>| match resolve_use(
            &cfg,
            &project_root_buf,
            &current,
            path,
            alias,
        ) {
            Ok(ResolvedImport::Npm {
                package,
                name,
                alias,
            }) => (package, name, alias),
            Ok(ResolvedImport::Local {
                rel_path,
                name,
                alias,
            }) => (rel_path, name, alias),
            Err(e) => {
                let placeholder = format!("/* psx error: {e} */");
                (
                    placeholder,
                    path.last().cloned().unwrap_or_default(),
                    alias.map(String::from),
                )
            }
        };
        if options.source_maps {
            // Path that downstream tools should resolve to find the .psx —
            // relative to where the .ts.map sits, which is the same dir as
            // the .ts. The .psx lives parallel under src/.
            let source_rel = relative_source_path(&out_path, psx_path);
            let generated_file = out_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("output.ts");
            let result = emit_with_source_map(
                module,
                &resolver,
                &traits,
                SourceMapInput {
                    source_path: &source_rel,
                    source_text: source,
                    generated_file,
                },
            );
            std::fs::write(&out_path, &result.ts)
                .with_context(|| format!("failed to write {}", out_path.display()))?;
            let map_path = out_path.with_file_name(&result.source_map_filename);
            std::fs::write(&map_path, &result.source_map_json)
                .with_context(|| format!("failed to write {}", map_path.display()))?;
        } else {
            let ts = emit_with_resolver_and_traits(module, &resolver, &traits);
            std::fs::write(&out_path, ts)
                .with_context(|| format!("failed to write {}", out_path.display()))?;
        }
        written.push((psx_path.clone(), out_path));
    }
    Ok(written)
}

/// Compute a `../`-relative path from `out_path`'s directory to `source`.
/// Falls back to the absolute path if the inputs don't share a prefix —
/// the source map is still valid, just less portable.
fn relative_source_path(out_path: &Path, source: &Path) -> String {
    let out_dir = out_path.parent().unwrap_or(out_path);
    let from: Vec<_> = out_dir.components().collect();
    let to: Vec<_> = source.components().collect();
    // Trim shared prefix.
    let mut shared = 0;
    while shared < from.len() && shared < to.len() && from[shared] == to[shared] {
        shared += 1;
    }
    let ups = from.len() - shared;
    let mut rel = PathBuf::new();
    for _ in 0..ups {
        rel.push("..");
    }
    for comp in &to[shared..] {
        rel.push(comp.as_os_str());
    }
    rel.to_string_lossy().into_owned()
}

fn collect_psx_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("psx") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}
