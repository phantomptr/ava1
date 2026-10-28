//! Paired devices, one per line: `<64 hex key> <unix seconds> <name>` (SPEC.md §5).
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_PEERS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub key: [u8; 32],
    pub added_unix: u64,
    pub name: String,
}

#[derive(Debug, Default)]
pub struct PeerStore {
    path: Option<PathBuf>,
    peers: Vec<Peer>,
    /// The file exists but could not be read: nothing is known, and nothing is ever
    /// written over it (that would unpair every device it lists).
    unreadable: Option<String>,
    /// Tokens this side stamped into helpers it launched (SPEC.md §5.2): a server that
    /// proves one is stored without a pairing code.
    launch: Option<crate::launch::LaunchTokens>,
}

impl PeerStore {
    /// Not saved anywhere (tests, one-off tools).
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// `load`, or — when the file exists but cannot be read — a store that knows no one,
    /// never writes the file, and reports why through `unreadable()`. A server keeps
    /// running on it but must not open its automatic pairing window (SPEC.md §5 item 7).
    pub fn load_or_unreadable(path: &Path) -> Self {
        Self::load(path).unwrap_or_else(|e| Self {
            path: Some(path.to_path_buf()),
            peers: Vec::new(),
            unreadable: Some(format!("{}: {e}", path.display())),
            launch: None,
        })
    }

    /// Why the peers file could not be read, if it could not.
    pub fn unreadable(&self) -> Option<&str> {
        self.unreadable.as_deref()
    }

    /// A missing file is an empty store; unreadable lines are skipped, never fatal.
    pub fn load(path: &Path) -> io::Result<Self> {
        let peers = match std::fs::read(path) {
            Ok(b) => String::from_utf8_lossy(&b)
                .lines()
                .filter_map(parse_line)
                .take(MAX_PEERS)
                .collect(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path: Some(path.to_path_buf()),
            peers,
            unreadable: None,
            launch: None,
        })
    }

    /// Recognise servers launched with one of these tokens (SPEC.md §5.2).
    pub fn with_launch_tokens(mut self, t: crate::launch::LaunchTokens) -> Self {
        self.launch = Some(t);
        self
    }

    /// Whether a Welcome's `launch_proof` on handshake `h` was made with a token this
    /// side issued and that has not expired.
    pub fn launched_by_us(&self, h: &[u8; 64], proof: &[u8; 16]) -> bool {
        self.launch.as_ref().is_some_and(|t| t.recognises(h, proof))
    }

    pub fn contains(&self, key: &[u8; 32]) -> bool {
        self.peers.iter().any(|p| &p.key == key)
    }

    pub fn list(&self) -> &[Peer] {
        &self.peers
    }

    fn refuse_if_unreadable(&self) -> io::Result<()> {
        match &self.unreadable {
            Some(why) => Err(io::Error::other(format!(
                "the peers file could not be read ({why}); not overwriting it"
            ))),
            None => Ok(()),
        }
    }

    /// Adds or replaces `key`; drops the oldest peer past `MAX_PEERS`; saves. Nothing
    /// changes unless the save succeeds.
    pub fn add(&mut self, key: [u8; 32], name: &str) -> io::Result<()> {
        self.refuse_if_unreadable()?;
        // Under the file's lock, from what is on disk now: another process may have paired a
        // device since this store was loaded, and saving the stale copy would unpair it.
        let _lock = self.lock_and_reload()?;
        let before = self.peers.clone();
        self.peers.retain(|p| p.key != key);
        if self.peers.len() >= MAX_PEERS {
            self.peers.remove(0);
        }
        let added_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.peers.push(Peer {
            key,
            added_unix,
            name: clean_name(name),
        });
        self.save().inspect_err(|_| self.peers = before)
    }

    /// Forgets `key`; saves. Whether it was there. Nothing changes unless the save
    /// succeeds.
    pub fn remove(&mut self, key: &[u8; 32]) -> io::Result<bool> {
        self.refuse_if_unreadable()?;
        let _lock = self.lock_and_reload()?;
        let before = self.peers.clone();
        self.peers.retain(|p| &p.key != key);
        if self.peers.len() == before.len() {
            return Ok(false);
        }
        self.save().inspect_err(|_| self.peers = before)?;
        Ok(true)
    }

    /// Takes the file's advisory lock (none for an in-memory store) and re-reads the peers
    /// from disk, so a read-modify-write starts from what every process has written.
    fn lock_and_reload(&mut self) -> io::Result<Option<crate::fslock::FileLock>> {
        let Some(path) = self.path.clone() else {
            return Ok(None);
        };
        let lock = crate::fslock::lock(&path)?;
        self.peers = Self::load(&path)?.peers;
        Ok(Some(lock))
    }

    /// Writes the file; the caller holds the lock.
    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text: String = self
            .peers
            .iter()
            .map(|p| {
                format!(
                    "{} {} {}\n",
                    crate::hex::encode(&p.key),
                    p.added_unix,
                    p.name
                )
            })
            .collect();
        let tmp = crate::fslock::TmpGuard::new(crate::fslock::unique_tmp(path));
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(tmp.path())?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(tmp.path(), path)?;
        tmp.disarm();
        Ok(())
    }
}

/// One line, no control characters, at most 63 bytes on a char boundary.
fn clean_name(n: &str) -> String {
    let mut s = String::new();
    for c in n.chars().map(|c| if c.is_control() { ' ' } else { c }) {
        if s.len() + c.len_utf8() > 63 {
            break;
        }
        s.push(c);
    }
    s
}

fn parse_line(l: &str) -> Option<Peer> {
    let mut it = l.splitn(3, ' ');
    let key: [u8; 32] = crate::hex::decode(it.next()?)?.try_into().ok()?;
    let added_unix = it.next()?.parse().ok()?;
    let name = it.next().unwrap_or("").to_string();
    Some(Peer {
        key,
        added_unix,
        name,
    })
}
