//! Compiles the AVA1 library's C (`c/`) on the host so `cargo test` can check it against Rust.
use std::path::PathBuf;

/// `AVA1_CTEST_SANITIZE=1` (review 009 #2a): every C unit this crate builds gets ASan + UBSan, so
/// the whole host-runnable library C runs instrumented under `cargo test`.
fn sanitize() -> bool {
    std::env::var("AVA1_CTEST_SANITIZE").as_deref() == Ok("1")
}

/// A `cc::Build` with the sanitizer flags when asked for.
fn build() -> cc::Build {
    let mut b = cc::Build::new();
    if sanitize() {
        b.flag("-fsanitize=address,undefined")
            .flag("-fno-sanitize-recover=undefined")
            .flag("-fno-omit-frame-pointer")
            .debug(true);
    }
    b
}

fn main() {
    println!("cargo:rerun-if-env-changed=AVA1_CTEST_SANITIZE");
    println!("cargo:rustc-check-cfg=cfg(ava1_ctest_sanitize)");
    if sanitize() {
        println!("cargo:rustc-link-arg=-fsanitize=address,undefined");
        // tests that measure the C's own thread stacks need to know ASan resizes them
        println!("cargo:rustc-cfg=ava1_ctest_sanitize");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        return; // POSIX C; the tests are #![cfg(unix)].
    }
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let c = here.join("../../c");
    let mono = c.join("third_party/monocypher");
    build()
        .file(mono.join("monocypher.c"))
        .include(&mono)
        .warnings(false)
        .compile("ava1third");
    let b3 = c.join("third_party/blake3");
    // Portable only on the host: the x86 assembly and NEON paths are the console build's concern;
    // the test pins the algorithm, and blake3_hash_many dispatches to the portable code.
    build()
        .files([
            b3.join("blake3.c"),
            b3.join("blake3_dispatch.c"),
            b3.join("blake3_portable.c"),
        ])
        .define("BLAKE3_NO_SSE2", None)
        .define("BLAKE3_NO_SSE41", None)
        .define("BLAKE3_NO_AVX2", None)
        .define("BLAKE3_NO_AVX512", None)
        .define("BLAKE3_USE_NEON", "0")
        .include(&b3)
        // -O2 like the console build (review P5): cargo's debug profile would otherwise
        // compile it at -O0, and the differential tests hash a lot.
        .opt_level(2)
        .warnings(false)
        .compile("ava1b3");
    build()
        .files([
            c.join("ava1_wire.c"),
            c.join("ava1_frame.c"),
            c.join("ava1_keys.c"),
            c.join("ava1_noise.c"),
            c.join("ava1_aead.c"),
            c.join("ava1_conn.c"),
            c.join("ava1_store.c"),
            c.join("ava1_pairlimit.c"),
            c.join("ava1_server.c"),
            c.join("ava1_trust.c"),
            c.join("ava1_ranges.c"),
            c.join("ava1_journal.c"),
            c.join("ava1_tune.c"),
            c.join("ava1_thread.c"),
            c.join("ava1_manifest.c"),
            c.join("ava1_job.c"),
            c.join("ava1_data.c"),
            c.join("ava1_apply.c"),
            c.join("ava1_recv.c"),
            c.join("ava1_send.c"),
            c.join("ava1_copy.c"),
            c.join("ava1_calibrate.c"),
            c.join("ava1_op.c"),
            c.join("ava1_events.c"),
            c.join("ava1_b3.c"),
            c.join("platform_posix.c"),
            c.join("gen/ava1_gen.c"),
            here.join("csrc/sizes.c"),
            here.join("csrc/test_shim.c"),
        ])
        .include(&c)
        .include(c.join("gen"))
        .include(&mono)
        .include(&b3)
        .include(here.join("csrc"))
        // blake3_impl.h must see the same configuration in ava1_b3.c as in the
        // ava1b3 build, whose blake3_hash_many it calls.
        .define("BLAKE3_NO_SSE2", None)
        .define("BLAKE3_NO_SSE41", None)
        .define("BLAKE3_NO_AVX2", None)
        .define("BLAKE3_NO_AVX512", None)
        .define("BLAKE3_USE_NEON", "0")
        .warnings(true)
        .extra_warnings(true)
        .warnings_into_errors(true)
        .compile("ava1c");
    // The AVX2 ChaCha20 is its own unit, built with -mavx2 only on x86-64 (where the
    // run-time CPUID check picks it); elsewhere it compiles to nothing.
    let mut avx2 = build();
    avx2.file(c.join("ava1_chacha_avx2.c"))
        .include(&c)
        .warnings(true)
        .extra_warnings(true)
        .warnings_into_errors(true);
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64") {
        avx2.flag("-mavx2");
    }
    avx2.compile("ava1avx2");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=pthread");
    }
    println!("cargo:rerun-if-changed={}", c.display());
    println!("cargo:rerun-if-changed={}", mono.display());
    println!("cargo:rerun-if-changed={}", b3.display());
    println!("cargo:rerun-if-changed=csrc");
}
