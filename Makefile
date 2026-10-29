# AVA1: build the C library, run every check. `make help` lists the targets.

CC ?= cc
CLANG ?= clang
AR ?= ar
BUILD ?= build

C := c
MONO := $(C)/third_party/monocypher
B3 := $(C)/third_party/blake3

# The library's own sources; platform_ps5.c needs the PS5 payload SDK and is built by such an
# application, not here.
LIB_SRCS := $(filter-out $(C)/platform_ps5.c $(C)/ava1_chacha_avx2.c,$(wildcard $(C)/*.c)) $(C)/gen/ava1_gen.c
LIB_OBJS := $(patsubst $(C)/%.c,$(BUILD)/obj/%.o,$(LIB_SRCS)) $(BUILD)/obj/ava1_chacha_avx2.o
DEP_OBJS := $(BUILD)/obj/monocypher.o $(BUILD)/obj/b3_blake3.o $(BUILD)/obj/b3_blake3_dispatch.o $(BUILD)/obj/b3_blake3_portable.o

# BLAKE3's SIMD kernels are left to applications that want them (see c/third_party/blake3);
# the portable code is what every target can build.
B3_DEFS := -DBLAKE3_NO_SSE2 -DBLAKE3_NO_SSE41 -DBLAKE3_NO_AVX2 -DBLAKE3_NO_AVX512 -DBLAKE3_USE_NEON=0
CFLAGS ?= -O2
WARN := -Wall -Wextra -Werror
INC := -I$(C) -I$(C)/gen -I$(MONO) -I$(B3)
ARCH := $(shell uname -m)

.PHONY: help lib example-c interop c-check test test-rust test-c ctest aead sanitize soak fuzz fuzz-c fuzz-rust \
	gen gen-check fmt fmt-check lint check package clean

help:
	@echo "make lib         build $(BUILD)/libava1.a (the C node library, POSIX)"
	@echo "make check       everything CI runs on a pull request, except fuzzing and sanitizers"
	@echo "make test        Rust tests, the C AEAD test, and the C<->Rust interop tests"
	@echo "make interop     the C example node and the Rust example client, end to end"
	@echo "make c-check     the C library with gcc and clang, -Wall -Wextra -Werror"
	@echo "make sanitize    the interop tests with ASan + UBSan (Linux)"
	@echo "make fuzz        the C and Rust decoders, 60 s each (clang + nightly)"
	@echo "make gen         regenerate the codecs from schema/ava1.toml"
	@echo "make package     cargo package (what crates.io would receive)"

# ---- the C library ----------------------------------------------------------------------------

lib: $(BUILD)/libava1.a

$(BUILD)/libava1.a: $(LIB_OBJS) $(DEP_OBJS)
	$(AR) rcs $@ $^

$(BUILD)/obj/%.o: $(C)/%.c $(wildcard $(C)/*.h) $(C)/gen/ava1_gen.h
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS) $(WARN) -std=gnu11 -pthread $(INC) $(B3_DEFS) -c -o $@ $<

# The AVX2 ChaCha20 is its own unit: -mavx2 on x86-64 only, where ava1_aead.c checks CPUID before
# it calls it; elsewhere it compiles to nothing.
$(BUILD)/obj/ava1_chacha_avx2.o: $(C)/ava1_chacha_avx2.c $(wildcard $(C)/*.h)
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS) $(WARN) -std=gnu11 $(if $(filter x86_64 amd64,$(ARCH)),-mavx2) $(INC) -c -o $@ $<

# Third-party code is not held to -Werror.
$(BUILD)/obj/monocypher.o: $(MONO)/monocypher.c
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS) -w -c -o $@ $<

$(BUILD)/obj/b3_%.o: $(B3)/%.c
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS) -w $(B3_DEFS) -c -o $@ $<

# The example node (examples/c/node.c) against the library.
example-c: $(BUILD)/ava1-node

$(BUILD)/ava1-node: examples/c/node.c $(BUILD)/libava1.a
	$(CC) $(CFLAGS) $(WARN) -std=gnu11 -pthread $(INC) -o $@ $< $(BUILD)/libava1.a

# The C example node and the Rust example client, paired over loopback, sending a folder.
interop: example-c
	scripts/interop-smoke.sh $(BUILD)/ava1-node

c-check:
	$(MAKE) clean-c BUILD=$(BUILD)/gcc
	$(MAKE) lib CC=gcc BUILD=$(BUILD)/gcc
	$(MAKE) lib CC=$(CLANG) BUILD=$(BUILD)/clang

.PHONY: clean-c
clean-c:
	rm -rf $(BUILD)/obj $(BUILD)/libava1.a

# ---- tests ------------------------------------------------------------------------------------

test: test-rust aead ctest

test-rust:
	cargo test --workspace --exclude ava1-ctest

# The C server and the data layer are process-wide singletons: one test at a time.
ctest:
	cargo test -p ava1-ctest -- --test-threads=1

aead:
	scripts/aead-test.sh

# The interop suite with ASan + UBSan on every C unit, LSan included on Linux (the Rust link of the
# sanitized C does not work with Homebrew's clang on macOS). lsan.supp lists the intentional leaks.
sanitize:
	CC=$(CLANG) AVA1_CTEST_SANITIZE=1 RUSTFLAGS="-Clinker=$(CLANG) $$RUSTFLAGS" \
		ASAN_OPTIONS=$${ASAN_OPTIONS:-detect_leaks=$$([ "$$(uname)" = Linux ] && echo 1 || echo 0)} \
		LSAN_OPTIONS=suppressions=$(CURDIR)/crates/ava1-ctest/lsan.supp \
		cargo test -p ava1-ctest -- --test-threads=1

# AVA1_SOAK_MINUTES=60 make soak
soak:
	AVA1_SOAK_MINUTES=$${AVA1_SOAK_MINUTES:-5} cargo test -p ava1-ctest --test soak -- --ignored --test-threads=1 --nocapture

# ---- fuzzing ----------------------------------------------------------------------------------

AVA1_FUZZ_SECONDS ?= 60
C_CODEC := $(C)/ava1_wire.c $(C)/ava1_frame.c $(C)/gen/ava1_gen.c $(C)/ava1_keys.c $(C)/ava1_noise.c \
	$(C)/ava1_aead.c $(C)/platform_posix.c $(MONO)/monocypher.c

fuzz: fuzz-c fuzz-rust

fuzz-c:
	@mkdir -p $(BUILD)
	$(CLANG) -g -O1 -fsanitize=fuzzer,address,undefined -DAVA1_AEAD_PORTABLE $(INC) \
		-o $(BUILD)/fuzz-decode $(C)/fuzz/fuzz_decode.c $(C_CODEC)
	$(BUILD)/fuzz-decode -max_total_time=$(AVA1_FUZZ_SECONDS) -max_len=65536
	$(CLANG) -g -O1 -fsanitize=fuzzer,address,undefined $(INC) $(B3_DEFS) \
		-o $(BUILD)/fuzz-data $(C)/fuzz/fuzz_data.c $(C)/ava1_manifest.c $(C)/ava1_journal.c \
		$(C)/ava1_ranges.c $(C)/ava1_wire.c $(C)/ava1_frame.c $(C)/gen/ava1_gen.c \
		$(B3)/blake3.c $(B3)/blake3_dispatch.c $(B3)/blake3_portable.c
	$(BUILD)/fuzz-data -max_total_time=$(AVA1_FUZZ_SECONDS) -max_len=65536

fuzz-rust:
	cd crates/ava1/fuzz && cargo +nightly fuzz run decode -- -max_total_time=$(AVA1_FUZZ_SECONDS)
	cd crates/ava1/fuzz && cargo +nightly fuzz run frame_header -- -max_total_time=$$(( $(AVA1_FUZZ_SECONDS) / 2 ))

# ---- code generation, formatting, lints ---------------------------------------------------------

gen:
	cargo run -q -p ava1-gen

gen-check:
	cargo run -q -p ava1-gen -- --check

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

check: fmt-check lint gen-check test c-check interop

package:
	cargo package -p ava1-gen
	cargo package -p ava1

clean:
	rm -rf $(BUILD)
	cargo clean
