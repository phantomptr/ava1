//! The identity file, the peer store and the launch-token file are shared by every
//! process that uses one data directory (the desktop app, its engine, the CLI tools): a
//! race must neither mint two identities nor lose a paired device or a token.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use ava1::keys::Identity;
use ava1::launch::{proof, LaunchTokens};
use ava1::peers::PeerStore;

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ava1-lock-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn leftovers(d: &Path) -> Vec<String> {
    std::fs::read_dir(d)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect()
}

#[test]
fn racing_threads_get_one_identity() {
    for round in 0..20 {
        let d = dir(&format!("id-t{round}"));
        let path = d.join("identity");
        let n = 8;
        let gate = Arc::new(Barrier::new(n));
        let hs: Vec<_> = (0..n)
            .map(|_| {
                let (path, gate) = (path.clone(), gate.clone());
                std::thread::spawn(move || {
                    gate.wait();
                    Identity::load_or_create(&path).unwrap().public()
                })
            })
            .collect();
        let keys: Vec<[u8; 32]> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(
            keys.iter().all(|k| k == &keys[0]),
            "round {round}: two identities were minted"
        );
        assert_eq!(Identity::load_or_create(&path).unwrap().public(), keys[0]);
        assert_eq!(
            leftovers(&d),
            Vec::<String>::new(),
            "no temp file is left behind"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn racing_threads_each_add_a_peer_and_none_is_lost() {
    for round in 0..10 {
        let d = dir(&format!("peers-t{round}"));
        let path = d.join("peers");
        let n = 8u8;
        let gate = Arc::new(Barrier::new(n as usize));
        let hs: Vec<_> = (0..n)
            .map(|i| {
                let (path, gate) = (path.clone(), gate.clone());
                std::thread::spawn(move || {
                    // Each thread has its own store, loaded before the others write: like
                    // separate processes holding stale copies.
                    let mut store = PeerStore::load(&path).unwrap();
                    gate.wait();
                    store.add([i + 1; 32], "peer").unwrap();
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let all = PeerStore::load(&path).unwrap();
        for i in 0..n {
            assert!(
                all.contains(&[i + 1; 32]),
                "round {round}: peer {i} was lost"
            );
        }
        assert_eq!(leftovers(&d), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[test]
fn racing_removes_and_adds_keep_the_file_consistent() {
    let d = dir("peers-rm");
    let path = d.join("peers");
    let mut seed = PeerStore::load(&path).unwrap();
    for i in 0..4u8 {
        seed.add([0x80 + i; 32], "old").unwrap();
    }
    let gate = Arc::new(Barrier::new(8));
    let hs: Vec<_> = (0..8u8)
        .map(|i| {
            let (path, gate) = (path.clone(), gate.clone());
            std::thread::spawn(move || {
                let mut s = PeerStore::load(&path).unwrap();
                gate.wait();
                if i < 4 {
                    s.remove(&[0x80 + i; 32]).unwrap();
                } else {
                    s.add([i; 32], "new").unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let all = PeerStore::load(&path).unwrap();
    for i in 0..4u8 {
        assert!(!all.contains(&[0x80 + i; 32]), "removal {i} was undone");
    }
    for i in 4..8u8 {
        assert!(all.contains(&[i; 32]), "add {i} was lost");
    }
}

#[test]
fn racing_token_issues_all_survive_and_each_token_is_spent_once() {
    let d = dir("tokens");
    let path = d.join("launch_tokens");
    let n = 8;
    let gate = Arc::new(Barrier::new(n));
    let hs: Vec<_> = (0..n)
        .map(|_| {
            let (path, gate) = (path.clone(), gate.clone());
            std::thread::spawn(move || {
                let t = LaunchTokens::at(&path);
                gate.wait();
                t.issue().unwrap()
            })
        })
        .collect();
    let tokens: Vec<[u8; 16]> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(LaunchTokens::at(&path).live(), n, "an issue was lost");
    // Every token is recognised exactly once, however many readers race for it.
    let h = [7u8; 64];
    let gate = Arc::new(Barrier::new(n));
    let hs: Vec<_> = tokens
        .iter()
        .map(|t| {
            let (path, gate, t) = (path.clone(), gate.clone(), *t);
            std::thread::spawn(move || {
                let a = LaunchTokens::at(&path);
                let b = LaunchTokens::at(&path);
                gate.wait();
                let p = proof(&t, &h);
                let (x, y) = std::thread::scope(|s| {
                    let x = s.spawn(|| a.recognises(&h, &p));
                    let y = s.spawn(|| b.recognises(&h, &p));
                    (x.join().unwrap(), y.join().unwrap())
                });
                u8::from(x) + u8::from(y)
            })
        })
        .collect();
    for h in hs {
        assert_eq!(
            h.join().unwrap(),
            1,
            "a token was spent twice (or not at all)"
        );
    }
    assert_eq!(LaunchTokens::at(&path).live(), 0);
    assert_eq!(leftovers(&d), Vec::<String>::new());
}

// ---- real processes: this test binary re-runs itself ----

const CHILD: &str = "AVA1_LOCK_CHILD_DIR";

/// Child body: only does anything when the parent set the environment.
#[test]
fn child_process_work() {
    let Ok(d) = std::env::var(CHILD) else { return };
    let d = PathBuf::from(d);
    let id = std::env::var("AVA1_LOCK_CHILD_ID")
        .unwrap()
        .parse::<u8>()
        .unwrap();
    // Wait for the starting gun so every child works at once.
    let go = d.join("go");
    while !go.exists() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let key = Identity::load_or_create(&d.join("identity"))
        .unwrap()
        .public();
    let mut peers = PeerStore::load(&d.join("peers")).unwrap();
    peers.add([id + 1; 32], "child").unwrap();
    println!("KEY {}", ava1::hex::encode(&key));
}

#[test]
fn racing_processes_get_one_identity_and_keep_every_peer() {
    let d = dir("procs");
    let me = std::env::current_exe().unwrap();
    let n = 6u8;
    let kids: Vec<_> = (0..n)
        .map(|i| {
            std::process::Command::new(&me)
                .args([
                    "--exact",
                    "child_process_work",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, &d)
                .env("AVA1_LOCK_CHILD_ID", i.to_string())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::write(d.join("go"), b"").unwrap();
    let mut keys = Vec::new();
    for k in kids {
        let out = k.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let key = text
            .lines()
            .find_map(|l| l.split_once("KEY ").map(|(_, k)| k.trim()))
            .unwrap_or_else(|| panic!("no key in {text}"))
            .to_string();
        keys.push(key);
    }
    assert!(
        keys.iter().all(|k| k == &keys[0]),
        "two identities were minted: {keys:?}"
    );
    let all = PeerStore::load(&d.join("peers")).unwrap();
    for i in 0..n {
        assert!(all.contains(&[i + 1; 32]), "peer {i} was lost");
    }
    assert_eq!(leftovers(&d), Vec::<String>::new());
}
