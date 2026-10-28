//! The lane frame-buffer pool (review 003 section 3): size classes, reuse without
//! zeroing, the idle cap, and no leak / double free.
#![cfg(unix)]
use ava1_ctest as _; // links the payload C
use std::os::raw::{c_int, c_void};
use std::sync::Mutex;

extern "C" {
    fn ava1_frame_cap(len: usize) -> usize;
    fn ava1_frame_pool_outstanding_bytes() -> u64;
    fn ava1_frame_alloc(len: usize, cap: *mut usize) -> *mut c_void;
    fn ava1_frame_free(p: *mut c_void, cap: usize) -> c_int;
    fn ava1_frame_pool_set_budget(bytes: u64);
    fn ava1_frame_pool_idle(cls: c_int) -> usize;
    fn ava1_frame_pool_outstanding() -> usize;
    fn ava1_frame_pool_trim();
}

const MIB: usize = 1 << 20;
/// The pool is one global: tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn fresh() -> std::sync::MutexGuard<'static, ()> {
    let g = TURN.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        ava1_frame_pool_set_budget(0);
        ava1_frame_pool_trim();
    }
    g
}

#[test]
fn only_a_class_sized_body_is_pooled_everything_else_is_exact() {
    unsafe {
        // Bodies below, between and above the classes: exact, never rounded up.
        assert_eq!(ava1_frame_cap(100), 100);
        assert_eq!(ava1_frame_cap(0), 1);
        assert_eq!(ava1_frame_cap(2 * MIB + 40), 2 * MIB + 40, "a 2 MiB chunk");
        assert_eq!(ava1_frame_cap(MIB - 1), MIB - 1);
        assert_eq!(ava1_frame_cap(5 * MIB), 5 * MIB);
        // A chunk of N MiB is N MiB plus its message header: that class.
        for c in [1, 4, 8, 15, 16] {
            assert_eq!(ava1_frame_cap(c * MIB), c * MIB);
            assert_eq!(ava1_frame_cap(c * MIB + 40), c * MIB);
            assert_eq!(ava1_frame_cap(c * MIB + 4096), c * MIB);
            assert_eq!(ava1_frame_cap(c * MIB + 4097), c * MIB + 4097);
        }
    }
}

#[test]
fn a_freed_class_buffer_is_reused_without_zeroing() {
    let _t = fresh();
    unsafe {
        let mut cap = 0usize;
        let p = ava1_frame_alloc(4 * MIB + 40, &mut cap) as *mut u8;
        assert!(!p.is_null());
        assert_eq!(cap, 4 * MIB);
        *p.add(4096) = 0xAB; // past the intrusive free-list link
        *p.add(4 * MIB + 100) = 0xCD; // the header slack is real, writable memory
        assert_eq!(ava1_frame_free(p as *mut c_void, cap), 0);
        assert_eq!(ava1_frame_pool_idle(1), 1);
        let q = ava1_frame_alloc(4 * MIB, &mut cap) as *mut u8;
        assert_eq!(q, p, "the same block comes back");
        assert_eq!(*q.add(4096), 0xAB, "and it was not zeroed");
        assert_eq!(ava1_frame_free(q as *mut c_void, cap), 0);
    }
}

#[test]
fn the_pool_never_holds_more_than_the_budget() {
    let _t = fresh();
    unsafe {
        ava1_frame_pool_set_budget(24 * MIB as u64);
        let mut cap = 0usize;
        let ps: Vec<_> = (0..10)
            .map(|_| ava1_frame_alloc(4 * MIB, &mut cap))
            .collect();
        for p in ps {
            assert_eq!(ava1_frame_free(p, cap), 0);
        }
        assert_eq!(ava1_frame_pool_idle(1), 6, "24 MiB / 4 MiB");
        let a = ava1_frame_alloc(16 * MIB, &mut cap);
        assert_eq!(ava1_frame_free(a, cap), 0);
        assert_eq!(
            ava1_frame_pool_idle(4),
            0,
            "16 MiB does not fit beside 24 MiB idle"
        );
        ava1_frame_pool_set_budget(8 * MIB as u64);
        assert_eq!(ava1_frame_pool_idle(1), 2, "shrinking the budget trims");
        ava1_frame_pool_set_budget(0);
    }
}

#[test]
fn each_class_obeys_budget_over_class() {
    let _t = fresh();
    unsafe {
        let mut cap = 0usize;
        let ps: Vec<_> = (0..9)
            .map(|_| ava1_frame_alloc(15 * MIB + 40, &mut cap))
            .collect();
        assert_eq!(cap, 15 * MIB);
        for p in ps {
            assert_eq!(ava1_frame_free(p, cap), 0);
        }
        assert_eq!(ava1_frame_pool_idle(3), 6, "96 MiB / 15 MiB");
    }
}

#[test]
fn every_buffer_is_counted_from_alloc_to_free_pooled_or_exact() {
    let _t = fresh();
    unsafe {
        let (base, base_bytes) = (
            ava1_frame_pool_outstanding(),
            ava1_frame_pool_outstanding_bytes(),
        );
        let lens = [1000, 2 * MIB + 40, 4 * MIB + 40, 5 * MIB];
        let mut held = Vec::new();
        for l in lens {
            let mut cap = 0usize;
            held.push((ava1_frame_alloc(l, &mut cap), cap));
        }
        assert_eq!(ava1_frame_pool_outstanding(), base + 4);
        assert!(ava1_frame_pool_outstanding_bytes() > base_bytes + (11 * MIB) as u64);
        for (p, cap) in held {
            assert_eq!(ava1_frame_free(p, cap), 0);
        }
        assert_eq!(
            ava1_frame_pool_outstanding(),
            base,
            "a leak check: back to base"
        );
        assert_eq!(ava1_frame_pool_outstanding_bytes(), base_bytes);
        // A refused double free leaves the counters alone and the list intact.
        let mut cap = 0usize;
        let p = ava1_frame_alloc(MIB, &mut cap);
        assert_eq!(ava1_frame_free(p, cap), 0);
        assert_eq!(ava1_frame_free(p, cap), -1);
        assert_eq!(ava1_frame_pool_outstanding(), base);
        assert_eq!(ava1_frame_pool_idle(0), 1);
        assert_eq!(ava1_frame_free(std::ptr::null_mut(), 0), 0);
    }
}

#[test]
fn threads_hammering_the_pool_balance_out() {
    let _t = fresh();
    let base = unsafe { ava1_frame_pool_outstanding() };
    let hs: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                for n in 0..200 {
                    let mut cap = 0usize;
                    let len = [100, MIB + 40, 3 * MIB, 8 * MIB + 40][(i + n) % 4];
                    unsafe {
                        let p = ava1_frame_alloc(len, &mut cap);
                        assert!(!p.is_null());
                        *(p as *mut u8).add(len - 1) = 1;
                        assert_eq!(ava1_frame_free(p, cap), 0);
                    }
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(unsafe { ava1_frame_pool_outstanding() }, base);
}

#[test]
fn a_window_of_two_mib_frames_costs_exactly_its_byte_count() {
    // The admit budget counts frame lengths; a non-class frame must cost its length, not a
    // rounded-up class (which doubled 2 MiB frames to 192 MiB against a 96 MiB budget).
    let _t = fresh();
    unsafe {
        let base = ava1_frame_pool_outstanding_bytes();
        let len = 2 * MIB + 40;
        let n = 96 * MIB / len;
        let held: Vec<_> = (0..n)
            .map(|_| {
                let mut cap = 0usize;
                (ava1_frame_alloc(len, &mut cap), cap)
            })
            .collect();
        assert_eq!(ava1_frame_pool_outstanding_bytes() - base, (n * len) as u64);
        assert!(ava1_frame_pool_outstanding_bytes() - base <= 96 * MIB as u64);
        for (p, cap) in held {
            assert_eq!(ava1_frame_free(p, cap), 0);
        }
        assert_eq!(ava1_frame_pool_outstanding_bytes(), base);
    }
}
