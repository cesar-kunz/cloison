#!/usr/bin/env bash
# Disassemble the JIT's main code for one dispatch_bench program.
#
#   scripts/jit-asm.sh int smi            # host (x86-64)
#   scripts/jit-asm.sh int smi aarch64    # arm64, run under qemu
#
# N (default 10) is the iteration count, ENGINE (default jit) jit, jit3, jit2 or jit1.
# The code shown is the last compiled: for `double` with N > 65536, the
# recompile after the int32 overflow exit.
set -euo pipefail
cd "$(dirname "$0")/.."
program=${1:?program}; codegen=${2:?codegen}; arch=${3:-x86_64}
dump=$(mktemp); trap 'rm -f "$dump"' EXIT
case "$arch" in
  x86_64)
    cargo build --release --quiet --bin dispatch_bench
    TOI_JIT_DUMP=$dump target/release/dispatch_bench "$program" "$codegen" "${N:-10}" "${ENGINE:-jit}" >/dev/null
    objdump -D -b binary -m i386:x86-64 -M intel --no-show-raw-insn "$dump" | sed -n '/<.data>:/,$p' ;;
  aarch64)
    cargo build --release --quiet --bin dispatch_bench --target aarch64-unknown-linux-musl
    TOI_JIT_DUMP=$dump qemu-aarch64 target/aarch64-unknown-linux-musl/release/dispatch_bench "$program" "$codegen" "${N:-10}" "${ENGINE:-jit}" >/dev/null
    aarch64-linux-gnu-objdump -D -b binary -m aarch64 --no-show-raw-insn "$dump" | sed -n '/<.data>:/,$p' ;;
  *) echo "usage: $0 <program> <codegen> [x86_64|aarch64]" >&2; exit 2 ;;
esac
