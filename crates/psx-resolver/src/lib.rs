//! PHPScript namespace resolver (PSR-4-style).
//!
//! Reads `psx.json`, walks the source tree, and resolves `use Foo\Bar;`
//! statements to file-relative ES module imports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Project configuration. Loaded from `psx.json` at the project root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PsxConfig {
    /// Base namespace for the project, e.g. `"App"`.
    pub namespace: String,
    /// Source directory, relative to the `psx.json` location. e.g. `"src"`.
    pub src: String,
    /// Output directory, relative to the `psx.json` location. Defaults to
    /// `"dist"`.
    #[serde(rename = "outDir", default = "default_out_dir")]
    pub out_dir: String,
    /// npm escape-hatch prefixes. Map of namespace prefix (e.g. `"Nd"` or
    /// `"Npm\\React"`) to literal npm package string. Longest-prefix match
    /// wins at resolution time.
    #[serde(rename = "npmPrefixes", default)]
    pub npm_prefixes: BTreeMap<String, String>,
}

fn default_out_dir() -> String {
    "dist".into()
}

impl PsxConfig {
    /// Read and parse a `psx.json` at the given path.
    pub fn load(path: &Path) -> Result<Self, ResolveError> {
        let bytes = std::fs::read(path).map_err(|e| ResolveError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let cfg: PsxConfig = serde_json::from_slice(&bytes).map_err(|e| ResolveError::Json {
            path: path.display().to_string(),
            source: e,
        })?;
        Ok(cfg)
    }

    /// Walk upward from `start` looking for a `psx.json`. Returns the
    /// directory containing the file (the project root) and the parsed
    /// config.
    pub fn discover_from(start: &Path) -> Option<(PathBuf, PsxConfig)> {
        let mut cur: Option<&Path> = Some(start);
        while let Some(dir) = cur {
            let candidate = dir.join("psx.json");
            if candidate.is_file() {
                if let Ok(cfg) = PsxConfig::load(&candidate) {
                    return Some((dir.to_path_buf(), cfg));
                }
                return None;
            }
            cur = dir.parent();
        }
        None
    }
}

/// The result of resolving one `use` item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedImport {
    /// `use Nd\Buffer;` -> `import { Request } from "@types/node";`
    Npm {
        package: String,
        name: String,
        alias: Option<String>,
    },
    /// `use App\Models\User;` -> `import { User } from "../Models/User";`
    Local {
        rel_path: String,
        name: String,
        alias: Option<String>,
    },
}

/// Resolve a single `use` item.
///
/// `path` is the dotted path segments without leading backslash, e.g.
/// `["App", "Models", "User"]`. `alias` is the optional `as <ident>`.
/// `current_file` is the absolute path of the source file containing the
/// `use` statement. `project_root` is the directory containing `psx.json`.
pub fn resolve_use(
    config: &PsxConfig,
    project_root: &Path,
    current_file: &Path,
    path: &[String],
    alias: Option<&str>,
) -> Result<ResolvedImport, ResolveError> {
    let full_path = path.join("\\");
    let name = path
        .last()
        .ok_or_else(|| ResolveError::UnresolvedUse {
            full_path: full_path.clone(),
            project_ns: config.namespace.clone(),
        })?
        .clone();

    // 1. Try longest npm prefix match.
    if let Some(pkg) = match_longest_npm_prefix(&config.npm_prefixes, path) {
        return Ok(ResolvedImport::Npm {
            package: pkg,
            name,
            alias: alias.map(String::from),
        });
    }

    // 2. PSR-4: the path must begin with the project's base namespace.
    let ns_segs: Vec<&str> = config.namespace.split('\\').collect();
    if path.len() <= ns_segs.len()
        || path[..ns_segs.len()]
            .iter()
            .zip(&ns_segs)
            .any(|(a, b)| a.as_str() != *b)
    {
        return Err(ResolveError::UnresolvedUse {
            full_path,
            project_ns: config.namespace.clone(),
        });
    }
    let sub_segs: Vec<&String> = path[ns_segs.len()..].iter().collect();

    // Target file (without extension): project_root/<src>/<sub_segs>.
    let mut target = project_root.join(&config.src);
    for seg in &sub_segs {
        target = target.join(seg.as_str());
    }

    // Relative from current_file's directory. Append `.js` so the emitted
    // TS round-trips through Node's ESM resolver — TypeScript accepts the
    // `.js` extension even when only the `.ts` source exists, and the
    // emitted JS keeps the literal path so `node` can find the file.
    let from_dir = current_file.parent().unwrap_or(project_root);
    let mut rel = relative_path(from_dir, &target);
    rel.push_str(".js");

    Ok(ResolvedImport::Local {
        rel_path: rel,
        name,
        alias: alias.map(String::from),
    })
}

/// Try to match `path` against the longest `npm_prefixes` key (where keys
/// are joined with `\\`). Returns the mapped package name on hit.
fn match_longest_npm_prefix(
    prefixes: &BTreeMap<String, String>,
    path: &[String],
) -> Option<String> {
    if prefixes.is_empty() {
        return None;
    }
    for take in (1..path.len()).rev() {
        // Path must have at least one segment after the prefix (the symbol name).
        let prefix = path[..take].join("\\");
        if let Some(pkg) = prefixes.get(&prefix) {
            return Some(pkg.clone());
        }
    }
    None
}

/// Compute a forward-slash-separated import path from `from_dir` to `target`.
/// Both should be absolute (or both relative against the same anchor). The
/// result is suitable for direct use in a TS `import { ... } from "..."`
/// statement and is prefixed with `./` for siblings to keep ES-module
/// resolvers happy.
fn relative_path(from_dir: &Path, target: &Path) -> String {
    let from: Vec<&std::ffi::OsStr> = from_dir.iter().collect();
    let to: Vec<&std::ffi::OsStr> = target.iter().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let ups = from.len() - common;
    let down = &to[common..];

    let mut parts: Vec<String> = Vec::with_capacity(ups + down.len());
    for _ in 0..ups {
        parts.push("..".into());
    }
    for c in down {
        parts.push(c.to_string_lossy().to_string());
    }
    if parts.is_empty() {
        return ".".into();
    }
    let joined = parts.join("/");
    if joined.starts_with("..") || joined.starts_with('/') {
        joined
    } else {
        format!("./{joined}")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("config not found at {path}")]
    ConfigNotFound { path: String },
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "use path {full_path} doesn't begin with project namespace {project_ns} \
         and doesn't match any npmPrefix"
    )]
    UnresolvedUse {
        full_path: String,
        project_ns: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let json = br#"{"namespace": "App", "src": "src"}"#;
        let cfg: PsxConfig = serde_json::from_slice(json).unwrap();
        assert_eq!(cfg.namespace, "App");
        assert_eq!(cfg.src, "src");
        assert_eq!(cfg.out_dir, "dist");
        assert!(cfg.npm_prefixes.is_empty());
    }

    #[test]
    fn parses_full_config() {
        let json = br#"{
            "namespace": "App",
            "src": "src",
            "outDir": "build",
            "npmPrefixes": {
                "Nd": "@types/node",
                "Npm\\React": "react"
            }
        }"#;
        let cfg: PsxConfig = serde_json::from_slice(json).unwrap();
        assert_eq!(cfg.out_dir, "build");
        assert_eq!(cfg.npm_prefixes.get("Nd"), Some(&"@types/node".to_string()));
        assert_eq!(
            cfg.npm_prefixes.get("Npm\\React"),
            Some(&"react".to_string())
        );
    }

    #[test]
    fn discover_walks_up_to_find_psx_json() {
        let tmp = tempdir_seed("discover-test");
        let project = tmp.join("project");
        let nested = project.join("src").join("Models");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            project.join("psx.json"),
            r#"{"namespace": "App", "src": "src"}"#,
        )
        .unwrap();
        let (root, cfg) = PsxConfig::discover_from(&nested).expect("discovers");
        assert_eq!(root, project);
        assert_eq!(cfg.namespace, "App");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discover_returns_none_when_no_psx_json_above() {
        let tmp = tempdir_seed("no-config-test");
        std::fs::create_dir_all(&tmp).unwrap();
        let result = PsxConfig::discover_from(&tmp);
        assert!(result.is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Tiny temp-directory helper. Avoids pulling in `tempfile` for one
    /// test case. Returns a unique path under the system temp dir.
    fn tempdir_seed(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "psx-resolver-test-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn cfg_with_npm() -> PsxConfig {
        let mut prefixes = BTreeMap::new();
        prefixes.insert("Nd".into(), "@types/node".into());
        prefixes.insert("Npm\\React".into(), "react".into());
        PsxConfig {
            namespace: "App".into(),
            src: "src".into(),
            out_dir: "dist".into(),
            npm_prefixes: prefixes,
        }
    }

    fn cfg_minimal() -> PsxConfig {
        PsxConfig {
            namespace: "App".into(),
            src: "src".into(),
            out_dir: "dist".into(),
            npm_prefixes: BTreeMap::new(),
        }
    }

    #[test]
    fn resolves_npm_single_segment_prefix() {
        let cfg = cfg_with_npm();
        let path = vec!["Nd".into(), "Buffer".into()];
        let r = resolve_use(
            &cfg,
            Path::new("/proj"),
            Path::new("/proj/src/Foo.psx"),
            &path,
            None,
        )
        .unwrap();
        assert_eq!(
            r,
            ResolvedImport::Npm {
                package: "@types/node".into(),
                name: "Buffer".into(),
                alias: None,
            }
        );
    }

    #[test]
    fn resolves_npm_multi_segment_prefix() {
        let cfg = cfg_with_npm();
        let path = vec!["Npm".into(), "React".into(), "useState".into()];
        let r = resolve_use(
            &cfg,
            Path::new("/proj"),
            Path::new("/proj/src/Foo.psx"),
            &path,
            None,
        )
        .unwrap();
        assert_eq!(
            r,
            ResolvedImport::Npm {
                package: "react".into(),
                name: "useState".into(),
                alias: None,
            }
        );
    }

    #[test]
    fn resolves_local_psr4_sibling() {
        let cfg = cfg_minimal();
        // current: /proj/src/Models/User.psx
        // target:  App\Models\Profile -> /proj/src/Models/Profile
        // relative: ./Profile.js
        let path = vec!["App".into(), "Models".into(), "Profile".into()];
        let r = resolve_use(
            &cfg,
            Path::new("/proj"),
            Path::new("/proj/src/Models/User.psx"),
            &path,
            None,
        )
        .unwrap();
        assert_eq!(
            r,
            ResolvedImport::Local {
                rel_path: "./Profile.js".into(),
                name: "Profile".into(),
                alias: None,
            }
        );
    }

    #[test]
    fn resolves_local_psr4_cross_directory() {
        let cfg = cfg_minimal();
        // current: /proj/src/Foo/Bar.psx
        // target:  App\Models\User -> /proj/src/Models/User
        // relative: ../Models/User.js
        let path = vec!["App".into(), "Models".into(), "User".into()];
        let r = resolve_use(
            &cfg,
            Path::new("/proj"),
            Path::new("/proj/src/Foo/Bar.psx"),
            &path,
            None,
        )
        .unwrap();
        assert_eq!(
            r,
            ResolvedImport::Local {
                rel_path: "../Models/User.js".into(),
                name: "User".into(),
                alias: None,
            }
        );
    }

    #[test]
    fn carries_alias_through() {
        let cfg = cfg_minimal();
        let path = vec!["App".into(), "Models".into(), "User".into()];
        let r = resolve_use(
            &cfg,
            Path::new("/proj"),
            Path::new("/proj/src/Foo.psx"),
            &path,
            Some("U"),
        )
        .unwrap();
        if let ResolvedImport::Local { alias, .. } = r {
            assert_eq!(alias.as_deref(), Some("U"));
        } else {
            panic!("expected Local");
        }
    }

    #[test]
    fn errors_on_use_path_outside_project_namespace() {
        let cfg = cfg_minimal();
        let path = vec!["Other".into(), "Pkg".into(), "Thing".into()];
        let err = resolve_use(
            &cfg,
            Path::new("/proj"),
            Path::new("/proj/src/Foo.psx"),
            &path,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::UnresolvedUse { .. }));
    }

    /// Longest-prefix wins when multiple npmPrefixes match.
    #[test]
    fn npm_prefix_longest_match_wins() {
        let mut prefixes = BTreeMap::new();
        prefixes.insert("Npm".into(), "generic-pkg".into());
        prefixes.insert("Npm\\React".into(), "react".into());
        let cfg = PsxConfig {
            namespace: "App".into(),
            src: "src".into(),
            out_dir: "dist".into(),
            npm_prefixes: prefixes,
        };
        let path = vec!["Npm".into(), "React".into(), "useState".into()];
        let r = resolve_use(
            &cfg,
            Path::new("/proj"),
            Path::new("/proj/src/Foo.psx"),
            &path,
            None,
        )
        .unwrap();
        if let ResolvedImport::Npm { package, .. } = r {
            assert_eq!(package, "react");
        } else {
            panic!("expected Npm");
        }
    }
}
