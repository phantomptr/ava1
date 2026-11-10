#![cfg(unix)]
use ava1::ranges::RangeSet;
use ava1::wire::SplitMix;
use ava1_ctest::*;

#[test]
fn c_range_sets_match_rust() {
    let mut rng = SplitMix(11);
    for _ in 0..300 {
        let ops: Vec<(u64, u64)> = (0..rng.below(40) + 1)
            .map(|_| {
                let s = rng.below(1000);
                (s, s + rng.below(60) + 1)
            })
            .collect();
        let mut r = RangeSet::new();
        for (s, e) in &ops {
            r.insert(*s, *e);
        }
        let want: Vec<(u64, u64)> = r.iter().collect();
        assert_eq!(c_rset_after(&ops), want, "{ops:?}");
    }
}

#[test]
fn c_range_sets_match_rust_at_the_boundaries() {
    // The randomized case above draws s < 1000 and len < 60: it never reaches the 1 MiB
    // group boundary, a 32/64-bit boundary, the top of the offset space, or a
    // zero-length op. Same oracle (the C shim merges the same ops independently),
    // boundary-biased draws plus deterministic hand cases.
    let edges = [
        0u64,
        1,
        (1 << 20) - 1,
        1 << 20,
        (1 << 32) - 1,
        1 << 32,
        1 << 63,
        u64::MAX - 1,
        u64::MAX,
    ];
    let lens = [0u64, 1, 2, (1 << 20) - 1, 1 << 20, 1 << 32, u64::MAX];
    let mut rng = SplitMix(13);
    let mut cases: Vec<Vec<(u64, u64)>> = vec![
        vec![],
        vec![(0, 0)],
        vec![(5, 5), (5, 6)],
        vec![(0, u64::MAX)],
        vec![(3, u64::MAX)],
        vec![(0, 1 << 20), (1 << 20, 2 << 20)],
        vec![(0, 2 << 20), (1 << 20, 2 << 20)],
        vec![(u64::MAX - 1, u64::MAX), (u64::MAX, u64::MAX)],
    ];
    for _ in 0..300 {
        let n = rng.below(20) + 1;
        let ops = (0..n)
            .map(|_| {
                let e = edges[rng.below(edges.len() as u64) as usize];
                let s = e.saturating_sub(rng.below(3));
                let l = lens[rng.below(lens.len() as u64) as usize];
                (s, s.saturating_add(l))
            })
            .collect();
        cases.push(ops);
    }
    for ops in cases {
        let mut r = RangeSet::new();
        for (s, e) in &ops {
            r.insert(*s, *e);
        }
        let want: Vec<(u64, u64)> = r.iter().collect();
        assert_eq!(c_rset_after(&ops), want, "{ops:?}");
    }
}

/// Removal (the read-back's to-do and done sets lose what a late write covers): against a plain bitmap,
/// with ranges that cut one pair in two, trim either end, swallow several pairs, or miss them all.
#[test]
fn c_range_set_removal_matches_a_bitmap() {
    const N: u64 = 300;
    let mut rng = SplitMix(14);
    for _ in 0..500 {
        let mut bits = [false; N as usize];
        let ops: Vec<(bool, u64, u64)> = (0..rng.below(30) + 1)
            .map(|_| {
                let s = rng.below(N);
                let e = (s + rng.below(40)).min(N);
                (rng.below(3) != 0, s, e)
            })
            .collect();
        for &(add, s, e) in &ops {
            for b in &mut bits[s as usize..e as usize] {
                *b = add;
            }
        }
        let mut want = Vec::new();
        let mut i = 0;
        while i < N as usize {
            if bits[i] {
                let s = i;
                while i < N as usize && bits[i] {
                    i += 1;
                }
                want.push((s as u64, i as u64));
            } else {
                i += 1;
            }
        }
        assert_eq!(c_rset_ops(&ops), Some(want), "{ops:?}");
    }
    // the edges of the offset space
    let m = u64::MAX;
    assert_eq!(
        c_rset_ops(&[(true, 0, m), (false, 1, m - 1)]),
        Some(vec![(0, 1), (m - 1, m)])
    );
    assert_eq!(c_rset_ops(&[(true, 5, 9), (false, 0, m)]), Some(vec![]));
    assert_eq!(
        c_rset_ops(&[(true, 5, 9), (false, 9, 12), (false, 3, 5)]),
        Some(vec![(5, 9)])
    );
    assert_eq!(
        c_rset_ops(&[(false, 3, 3), (true, 1, 4), (false, 2, 2)]),
        Some(vec![(1, 4)])
    );
}

#[test]
fn c_file_runs_match_rust() {
    let mut rng = SplitMix(12);
    for _ in 0..200 {
        let n = rng.below(500) as u32 + 1;
        let set: std::collections::BTreeSet<u32> = (0..n).filter(|_| rng.below(3) == 0).collect();
        let rust = ava1::ranges::runs(&set);
        let c = c_bits_runs(n, &set.iter().copied().collect::<Vec<_>>());
        assert_eq!(
            c,
            rust.iter().map(|r| (r.first, r.count)).collect::<Vec<_>>()
        );
    }
}
