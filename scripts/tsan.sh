#!/usr/bin/env bash
# The tests under ThreadSanitizer (x86-64 Linux; the pinned nightly with
# its `rust-src`, for `-Zbuild-std`): races between the mutator, the
# concurrent marker and agents' threads, in the Rust code. Not in
# compiled code or `meta_word`'s asm, which TSan doesn't see: the JIT is
# off (`CLOISON_NO_JIT`) but where a test sets the tier, as tests/js.rs's
# and tests/scripts/'s do (the stencils are built for this layout too). The heap's
# reservation is laid out for TSan's memory (`cloison_tsan`, see
# build.rs). Exits non-zero on a report or a failure; scripts/tsan.supp
# holds the races known benign.
#
#   scripts/tsan.sh                        # the unit tests, tests/heaps.rs,
#                                          # tests/js.rs, tests/scripts/
#   scripts/tsan.sh --test heaps agent     # cargo test's arguments
#   CYCLES=1 scripts/tsan.sh               # with CLOISON_GC_CYCLES=1:
#                                          # collections back to back
set -uo pipefail
cd "$(dirname "$0")/.."
args=("$@")
# (16 test threads: the heaps of 8 GiB that fit are 128, and an agent
# test takes 65.)
[ $# -gt 0 ] || args=(--lib --test heaps --test js --test scripts -- --test-threads=16)
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-target/tsan}
export RUSTFLAGS="-Zsanitizer=thread"
export CLOISON_NO_JIT=1
export TSAN_OPTIONS="halt_on_error=0 exitcode=66 second_deadlock_stack=1 suppressions=$PWD/scripts/tsan.supp ${TSAN_OPTIONS:-}"
[ -z "${CYCLES:-}" ] || export CLOISON_GC_CYCLES=$CYCLES
# (The test harness unwinds; `panic_abort`, built from source, would not
# match the release profile's `panic = "abort"`.)
cargo test -Zbuild-std --target x86_64-unknown-linux-gnu --release \
  --config 'profile.release.panic="unwind"' "${args[@]}"
