#![cfg(unix)]
use ava1::verify::{group_cv, root_from_cvs, GROUP};
use ava1::wire::SplitMix;
use ava1_ctest::*;

#[test]
fn c_group_cvs_and_roots_match_rust() {
    let mut rng = SplitMix(5);
    let g = GROUP as usize;
    let mut sizes = vec![
        g + 1,
        2 * g,
        2 * g + 1,
        3 * g - 1,
        4 * g + 1024,
        4 * g + 1023,
        9 * g + 64,
    ];
    for _ in 0..12 {
        // g + 1 + [0, 12g - 1): every size covers at least two groups, so the Rust
        // root_from_cvs (and the C contract) never sees a one-group file.
        sizes.push(g + 1 + rng.below(12 * GROUP - 1) as usize);
    }
    for n in sizes {
        let mut d = vec![0u8; n];
        rng.fill(&mut d);
        let cvs: Vec<[u8; 32]> = d
            .chunks(g)
            .enumerate()
            .map(|(i, x)| group_cv(x, i as u64))
            .collect();
        for (i, x) in d.chunks(g).enumerate() {
            assert_eq!(c_b3_group_cv(x, i as u64), cvs[i], "size {n} group {i}");
        }
        assert_eq!(c_b3_root(&cvs), root_from_cvs(&cvs), "size {n}");
        assert_eq!(c_b3_hash(&d), *blake3::hash(&d).as_bytes(), "size {n}");
    }
}

#[test]
fn c_hash_matches_rust_for_files_of_at_most_one_group() {
    // §13: a file of zero or one group has no group CVs — its root is BLAKE3 over the
    // whole bytes, which is the path ava1_b3_hash exists for. The sizes above all cover
    // >= 2 groups, so without this the wrapper would never run.
    let mut rng = SplitMix(6);
    let g = GROUP as usize;
    for n in [0, 1, 1024, g / 2, g - 1, g] {
        let mut d = vec![0u8; n];
        rng.fill(&mut d);
        assert_eq!(c_b3_hash(&d), *blake3::hash(&d).as_bytes(), "size {n}");
    }
}
