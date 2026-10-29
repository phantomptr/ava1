//! The C library must never use the wall clock for timing: a settimeofday on a device (a date
//! sync) once looked like a resume and woke an application's wake watchdog.
use std::path::Path;

/// Every .c and .h under `dir`, subdirectories (gen/, fuzz/) included.
fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            sources(&p, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("c" | "h")) {
            out.push(p);
        }
    }
}

#[test]
fn the_c_library_uses_only_monotonic_clocks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    sources(&root.join("c"), &mut files);
    // Vendored libraries are not ours to police.
    let c = root.join("c");
    files.retain(|p| !p.strip_prefix(&c).unwrap().starts_with("third_party"));
    for p in &files {
        let src = std::fs::read_to_string(p).unwrap();
        for bad in ["gettimeofday", "CLOCK_REALTIME", "clock_settime", "ftime("] {
            // Whole identifiers only: strftime( formats a display timestamp (the event log), not timing.
            let hit = src.match_indices(bad).any(|(i, _)| {
                i == 0
                    || !src.as_bytes()[i - 1].is_ascii_alphanumeric()
                        && src.as_bytes()[i - 1] != b'_'
            });
            assert!(!hit, "{} uses {bad}; use CLOCK_MONOTONIC", p.display());
        }
    }
    let count = |ext: &str, under: &str| {
        files
            .iter()
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some(ext))
            .filter(|p| p.to_string_lossy().contains(under))
            .count()
    };
    assert!(
        count("c", "ava1") >= 9,
        "found only {} C files",
        count("c", "ava1")
    );
    assert!(
        count("h", "ava1") >= 8,
        "found only {} headers",
        count("h", "ava1")
    );
    assert!(
        count("c", "/gen/") >= 1 && count("h", "/gen/") >= 1,
        "the generated sources under gen/ were not scanned"
    );
}

/// Review 006 #1: a re-key restarts the AEAD counter at 0, so it may only happen where a
/// connection gets its keys (handshake, lane join): never from the data plane. This pins
/// every non-test `set_key` call site; a new one must be audited (AUDIT-nonce.md) first.
#[test]
fn set_key_is_called_only_where_a_connection_is_keyed() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sites = Vec::new();
    for e in std::fs::read_dir(&src).unwrap() {
        let p = e.unwrap().path();
        let text = std::fs::read_to_string(&p).unwrap();
        // Production code only: drop the file's `#[cfg(test)] mod tests { .. }` block (a test-only
        // accessor earlier in the file must not hide the code after it).
        let prod = match text.find("#[cfg(test)]\nmod tests") {
            Some(i) => &text[..i],
            None => &text[..],
        };
        let n = prod.matches(".set_key(").count();
        if n > 0 {
            sites.push((p.file_name().unwrap().to_string_lossy().into_owned(), n));
        }
    }
    sites.sort();
    assert_eq!(
        sites,
        vec![
            ("handshake.rs".to_string(), 4),
            ("server.rs".to_string(), 2),
            ("session.rs".to_string(), 2),
        ]
    );
}

/// The C side: a send/recv counter is written only at its increment (and zeroed with the
/// connection); no other code may assign it.
#[test]
fn c_counters_are_only_incremented() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    sources(&root.join("c"), &mut files);
    let c = root.join("c");
    files.retain(|p| !p.strip_prefix(&c).unwrap().starts_with("third_party"));
    for p in &files {
        let src = std::fs::read_to_string(p).unwrap();
        for (n, line) in src.lines().enumerate() {
            for f in ["send_ctr", "recv_ctr"] {
                if let Some(i) = line.find(f) {
                    let rest = line[i + f.len()..].trim_start();
                    // `=`, `+=`, `-=`, `|=` and the like; `++` (the increment) is the only write.
                    let op = rest.trim_start_matches(['+', '-', '*', '/', '|', '&', '^']);
                    let compound = op.len() != rest.len() && op.starts_with('=');
                    let writes = compound || (rest.starts_with("= ") && !rest.starts_with("== "));
                    assert!(!writes, "{}:{} assigns {f}: {line}", p.display(), n + 1);
                }
            }
        }
    }
}
