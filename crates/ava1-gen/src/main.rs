//! `ava1-gen`: regenerate the core codecs (no arguments, from this repository), or generate an
//! application's extension:
//!
//! ```text
//! ava1-gen --ext app --schema app.toml --rust src/app_gen.rs --c-dir payload/gen \
//!          [--source "protocol/app.toml" --regenerate "run `make gen`"] [--check]
//! ```
//!
//! `--check` writes nothing and fails when a file differs from what would be generated.
use std::path::{Path, PathBuf};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let check = args.iter().any(|a| a == "--check");
    let outputs = match opt("--ext") {
        None => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            ava1_gen::outputs(&root)?
        }
        Some(ext) => {
            let schema_path = opt("--schema").ok_or("--schema <file> is required with --ext")?;
            let rust = PathBuf::from(opt("--rust").ok_or("--rust <file.rs> is required")?);
            let c_dir = PathBuf::from(opt("--c-dir").ok_or("--c-dir <dir> is required")?);
            let source = opt("--source").unwrap_or_else(|| schema_path.clone());
            let regenerate = opt("--regenerate").unwrap_or_else(|| "run ava1-gen".into());
            let text =
                std::fs::read_to_string(&schema_path).map_err(|e| format!("{schema_path}: {e}"))?;
            let t = ava1_gen::Target {
                source: &source,
                regenerate: &regenerate,
                ext: Some(&ext),
            };
            ava1_gen::extension_outputs(&text, &t, &rust, &c_dir)?
        }
    };
    let mut stale = Vec::new();
    for (path, text) in outputs {
        if check {
            if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
                stale.push(path.display().to_string());
            }
            continue;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }
    if stale.is_empty() {
        Ok(())
    } else {
        Err(format!("stale (run ava1-gen): {}", stale.join(", ")))
    }
}
