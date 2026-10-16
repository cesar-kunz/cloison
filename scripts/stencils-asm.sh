#!/usr/bin/env bash
# Disassemble the stencils as compiled by build.rs, with their relocations
# (the holes the JIT patches).
#
#   scripts/stencils-asm.sh            # host (x86-64)
#   scripts/stencils-asm.sh aarch64    # arm64
#
# This is the raw object: on arm64, build.rs then folds each trailing
# `b <helper>` stub into the conditional branches that target it.
set -euo pipefail
cd "$(dirname "$0")/.."
case "${1:-x86_64}" in
  x86_64)  triple=x86_64-unknown-linux-gnu;   objdump=(objdump -M intel) ;;
  aarch64) triple=aarch64-unknown-linux-musl; objdump=(aarch64-linux-gnu-objdump) ;;
  *) echo "usage: $0 [x86_64|aarch64]" >&2; exit 2 ;;
esac
cargo build --release --quiet --lib --target "$triple"
dir=target/$triple/release
obj=$(ls -t "$dir"/build/cloison/*/out/stencils.o | head -1)
"${objdump[@]}" -dr --no-show-raw-insn "$obj" | awk '/^[0-9a-f]+ <cloison_stencil_/ {p = 1} /^$/ {if (p) print ""; p = 0} p'
