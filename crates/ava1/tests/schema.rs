//! The data-plane layout rules (SPEC.md §11–§12).
use ava1::gen::*;
use ava1::wire::{FrameMessage, Message};

const J: [u8; 16] = [7; 16];

fn first16<M: FrameMessage>(m: M) {
    let b = m.to_bytes().unwrap();
    assert_eq!(&b[..16], &J, "{} must start with job_id", M::NAME);
    assert!((0x20..=0x3F).contains(&M::TYPE), "{} type", M::NAME);
}

#[test]
fn every_data_message_starts_with_the_job_id() {
    first16(JobOpen {
        job_id: J,
        ..Default::default()
    });
    first16(JobOpenAck {
        job_id: J,
        ..Default::default()
    });
    first16(ManifestPage {
        job_id: J,
        ..Default::default()
    });
    first16(ManifestEnd {
        job_id: J,
        ..Default::default()
    });
    first16(JobMap {
        job_id: J,
        ..Default::default()
    });
    first16(Resume {
        job_id: J,
        ..Default::default()
    });
    first16(Chunk {
        job_id: J,
        ..Default::default()
    });
    first16(Bundle {
        job_id: J,
        ..Default::default()
    });
    first16(Received {
        job_id: J,
        ..Default::default()
    });
    first16(Credit {
        job_id: J,
        ..Default::default()
    });
    first16(Durable {
        job_id: J,
        ..Default::default()
    });
    first16(FileRoot {
        job_id: J,
        ..Default::default()
    });
    first16(FileRetry {
        job_id: J,
        ..Default::default()
    });
    first16(Status {
        job_id: J,
        ..Default::default()
    });
    first16(JobDone {
        job_id: J,
        ..Default::default()
    });
    first16(JobCancel {
        job_id: J,
        ..Default::default()
    });
}

#[test]
fn a_full_manifest_page_fits_a_control_frame() {
    // 60 KiB of entries with 1 KiB paths, with the page message's own fields in the slack.
    let e = ManifestEntry {
        path: "p".repeat(1024),
        ..Default::default()
    };
    let per = e.to_bytes().unwrap().len() + 4;
    let n = (60 * 1024 - 64) / per;
    let page = ManifestPage {
        job_id: J,
        entries: vec![e; n],
    };
    let b = page.to_bytes().unwrap();
    // A page rides the control connection, which is sealed from Hs3 on (§5), and the cap
    // is on `body_len`, which counts the MAC (§2, §4.4) — so the body may use
    // CONTROL_MAX_BODY - MAC_LEN.
    assert!(
        b.len() + ava1::keys::MAC_LEN <= ava1::frame::CONTROL_MAX_BODY as usize,
        "{}",
        b.len()
    );
    assert_eq!(ManifestPage::decode(&b).unwrap().entries.len(), n);
}

/// The numbers that travel on the wire, pinned so an edit to the schema cannot silently
/// renumber what a peer already understands. The SPEC states some of these in prose (§1
/// port 9120, §9 eight lanes, §12.2 one-MiB groups, §11.3 job kinds and entry kinds,
/// §11.4 policies, 1 = node.info in §7); the rest are the schema's own numbering, which
/// both implementations are generated from — this test is the tripwire, not a second
/// source of truth.
#[test]
fn the_wire_numbers_are_pinned() {
    assert_eq!(DEFAULT_PORT, 9120); // §1
    assert_eq!(MAX_LANES, 8); // §9: lanes 1..=8
    assert_eq!(1u64 << GROUP_SHIFT, 1 << 20); // §12.2: one verification group
    assert_eq!(METHOD_NODE_INFO, 1); // §7
    assert_eq!(METHOD_PAIRING_OPEN, 2); // §5: pairing.open
    assert_eq!((JOB_UPLOAD, JOB_DOWNLOAD, JOB_COPY), (1, 2, 3)); // §11.3
    assert_eq!(
        (POLICY_REPLACE, POLICY_SKIP_EXISTING, POLICY_VERIFY),
        (0, 1, 2)
    ); // §11.4
    assert_eq!((ENTRY_FILE, ENTRY_DIR), (0, 1)); // §11.3
    assert_eq!(JF_SINGLE_FILE, 1); // §11.6
    assert_eq!((STATUS_OK, CAP_DATA_PLANE), (0, 1)); // §7, §10
    assert_eq!((RETRY_VERIFY, RETRY_IO, RETRY_CHANGED), (1, 2, 3));
    // Statuses travel in `RpcResponse.status` and as `Error.code`: renumbering these is a
    // protocol break.
    let errors: [u16; 17] = [
        ERR_NOT_PAIRED,
        ERR_PAIRING_CLOSED,
        ERR_UNSUPPORTED_VERSION,
        ERR_PROTOCOL,
        ERR_BAD_JOIN,
        ERR_UNKNOWN_METHOD,
        ERR_INTERNAL,
        ERR_BUSY,
        ERR_PATH,
        ERR_NO_SPACE,
        ERR_UNKNOWN_JOB,
        ERR_IO,
        ERR_VERIFY,
        ERR_EXISTS,
        ERR_CANCELLED,
        ERR_CROSS_DEVICE,
        ERR_CREDIT,
    ];
    assert_eq!(
        errors,
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17]
    );
}

// ── Management bodies (SPEC.md §7.3) ─────────────────────────────────────────

fn status_with_result() -> Status {
    Status {
        job_id: J,
        files_done: 1,
        files_total: 1,
        bytes_received: 0,
        bytes_durable: 0,
        bytes_total: 0,
        bottleneck: 0,
        workers: 1,
        lanes: 0,
        sequential: 0,
        current: None,
        state: Some(2),
        result: Some(b"cause".to_vec()),
        code: Some(ERR_IO),
        unswept: None,
    }
}

#[test]
fn management_structs_round_trip() {
    use ava1::wire::Message;
    let t = MgmtText {
        body: b"{\"ok\":true}".to_vec(),
        more: Some(1),
    };
    assert_eq!(MgmtText::decode(&t.to_bytes().unwrap()).unwrap(), t);
    let j = JobRun {
        job_id: J,
        op: 1, // the application's operation number
        args: b"{\"path\":\"/data/x\"}".to_vec(),
    };
    assert_eq!(JobRun::decode(&j.to_bytes().unwrap()).unwrap(), j);
    let jl = JobListResult {
        jobs: vec![JobEntry {
            job_id: J,
            kind: JOB_COPY,
            state: 0,
            files_done: 1,
            files_total: 9,
            bytes_done: 5,
            bytes_total: 50,
        }],
    };
    assert_eq!(JobListResult::decode(&jl.to_bytes().unwrap()).unwrap(), jl);
    let sr = status_with_result();
    assert_eq!(Status::decode(&sr.to_bytes().unwrap()).unwrap(), sr);
}

#[test]
fn every_core_method_constant_is_unique_and_pinned() {
    let src = include_str!("../../../schema/ava1.toml");
    let mut seen = std::collections::BTreeMap::new();
    let mut name = String::new();
    for line in src.lines() {
        if let Some(n) = line.strip_prefix("name = \"METHOD_") {
            name = format!("METHOD_{}", n.split('"').next().unwrap());
        } else if let Some(v) = line.strip_prefix("value = ") {
            if !name.is_empty() {
                let v: u32 = v.trim().parse().unwrap();
                // 1-3 and 16-31 are the protocol's; 4-15 and 32+ are the application's (§7.3).
                assert!(
                    (1..=3).contains(&v) || (16..=31).contains(&v),
                    "{name} = {v} is in the application's range"
                );
                if let Some(prev) = seen.insert(v, name.clone()) {
                    panic!("{name} and {prev} are both method {v}");
                }
                name.clear();
            }
        } else if line.starts_with("name = ") {
            name.clear();
        }
    }
    for (n, v) in [
        ("node.info", 1),
        ("pairing.open", 2),
        ("job.copy", 16),
        ("job.run", 20),
        ("job.list", 21),
        ("c2c.send", 23),
    ] {
        let c = format!("METHOD_{}", n.to_uppercase().replace('.', "_"));
        assert_eq!(seen.get(&v), Some(&c));
    }
}

#[test]
fn the_rpc_limits_are_in_the_spec_and_the_code() {
    let spec = include_str!("../../../spec/SPEC.md");
    assert!(spec.contains("at most 8 requests in flight"));
    assert!(spec.contains("256 KiB") && spec.contains("56 KiB"));
    assert_eq!(ava1::server::RPC_WORKERS, 8);
    assert_eq!(ava1::server::RPC_REPLY_MAX, 256 * 1024);
    assert_eq!(ava1::server::RPC_REQUEST_MAX, 56 * 1024);
}

#[test]
fn the_text_limit_agrees_with_the_rpc_caps() {
    // MgmtText overhead is 6 bytes, 13 with `more`.
    assert_eq!(RPC_TEXT_MAX as usize, ava1::server::RPC_REPLY_MAX - 16);
    let worst = MgmtText {
        body: vec![b'x'; RPC_TEXT_MAX as usize],
        more: Some(1),
    }
    .to_bytes()
    .unwrap();
    assert!(worst.len() <= ava1::server::RPC_REPLY_MAX);
}
