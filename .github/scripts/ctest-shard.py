#!/usr/bin/env python3
"""Deal the ava1-ctest test files across CI shards by measured run time.

Usage: ctest-shard.py SHARD COUNT  (SHARD is 1-based)

Prints `--test <name>` argument pairs, one token per line, for cargo test.

Used by the ctest and sanitize jobs in .github/workflows/ci.yml. Both used
to deal the files round-robin by name, which left shard 3 with ~300 s of tests against ~170-240 s
on the others, so the slowest shard set the PR gate's time. Here the files are dealt longest
first, each to the shard with the least time so far (the classic LPT schedule), so the four
shards come out within a few seconds of each other.

Every shard computes the same assignment from the same sorted file list, so the shards together
still run every file exactly once. A file missing from WEIGHTS (added after the table was
measured) still runs, under DEFAULT_WEIGHT; refresh the table when the shards drift apart.
"""

import pathlib
import sys

# Seconds per test binary (link + run) under `cargo test -- --test-threads=1`, the mean of the
# plain and the ASan + UBSan runs on ubuntu-24.04, measured from CI runs 40079393365,
# 40082659548 and 40084135077 (2026-10-24).
WEIGHTS = {
    "admission": 0, "aead": 3, "apply": 18, "apply_perf": 23, "apply_timing": 1, "b3": 13,
    "c2c": 25, "conformance": 1, "copy": 23, "data_hooks": 1, "diag": 3, "download": 59,
    "download_trust": 0, "fan_map": 0, "fdlimit": 59, "firmware": 0, "fix_round": 18,
    "fix_round2": 2, "frame_budget": 40, "frame_pool": 0, "governor": 69, "job_run": 16,
    "job_run_trust": 0, "jobs_any_owner": 13, "journal": 0, "lane_sockbuf": 0,
    "lf_slots_wedge": 91, "lifecycle": 1, "live_notes": 2, "log_small": 10,
    "log_small_engine": 2, "log_small_fail": 31, "log_small_settle": 4, "log_small_upload": 1,
    "manifest": 0, "many_jobs_one_session": 13, "mgmt": 4, "mgmt_fs": 9, "mgmt_t6": 6,
    "mgmt_t7": 6, "nas_policy": 0, "nas_skip": 6, "noise": 1, "pairing": 4,
    "payload_cutover": 0, "perf_upload": 0, "periodic_kill": 6, "ranges": 0, "recv": 16,
    "resume_after_cancel": 50, "rp_pair": 0, "scaling": 26, "send": 4, "send_timing": 0,
    "server": 39, "sevenz": 6, "shutdown": 9, "single_instance": 1, "soak": 10,
    "space_resume": 49, "stall": 18, "torn_sweep": 53, "trust": 0, "tune": 0, "upload": 77,
    "wire_upload": 16,
}
DEFAULT_WEIGHT = 15
# Each file is also its own test binary to compile and link before anything runs, which the
# measured times above leave out; charge it so the shard with the many tiny files is not overfilled.
PER_FILE_COST = 2

TESTS_DIR = pathlib.Path(__file__).resolve().parents[2] / "crates" / "ava1-ctest" / "tests"


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    shard, count = int(sys.argv[1]), int(sys.argv[2])
    if not 1 <= shard <= count:
        print(f"shard {shard} is outside 1..{count}", file=sys.stderr)
        return 2

    names = sorted(p.stem for p in TESTS_DIR.glob("*.rs"))
    if not names:
        print(f"no test files under {TESTS_DIR}", file=sys.stderr)
        return 1
    unweighted = [n for n in names if n not in WEIGHTS]
    if unweighted:
        print(f"::notice::ctest-shard: no measured time for {', '.join(unweighted)}; "
              f"assuming {DEFAULT_WEIGHT} s each", file=sys.stderr)

    loads = [0] * count
    assigned = [[] for _ in range(count)]
    # Longest first; ties by name so every shard computes the same deal.
    for name in sorted(names, key=lambda n: (-WEIGHTS.get(n, DEFAULT_WEIGHT), n)):
        i = min(range(count), key=lambda k: (loads[k], k))
        loads[i] += WEIGHTS.get(name, DEFAULT_WEIGHT) + PER_FILE_COST
        assigned[i].append(name)

    mine = sorted(assigned[shard - 1])
    if not mine:
        print(f"::error::shard {shard} got no test files", file=sys.stderr)
        return 1
    print(f"shard {shard} of {count}: {len(mine)} of {len(names)} test files, "
          f"~{loads[shard - 1]} s estimated (shards: {', '.join(f'{x} s' for x in loads)})",
          file=sys.stderr)
    for name in mine:
        print("--test")
        print(name)
    return 0


if __name__ == "__main__":
    sys.exit(main())
