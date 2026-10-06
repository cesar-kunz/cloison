#!/usr/bin/env bash
# Build release and dump every opcode handler's machine code, with instruction
# counts (padding excluded). Slow paths (`slow_*`) are listed too.
# `op_nop` is pure dispatch, so its count is the dispatch length. Exits
# non-zero if it exceeds TARGET (default 3, the floor on both architectures).
#
#   scripts/dispatch-asm.sh            # host (x86-64)
#   scripts/dispatch-asm.sh aarch64    # arm64, cross-built for musl
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET=${TARGET:-3}
case "${1:-x86_64}" in
  x86_64)  triple=x86_64-unknown-linux-gnu;   objdump=(objdump -C -M intel) ;;
  aarch64) triple=aarch64-unknown-linux-musl; objdump=(aarch64-linux-gnu-objdump -C) ;;
  *) echo "usage: $0 [x86_64|aarch64]" >&2; exit 2 ;;
esac
cargo build --release --quiet --bin dispatch_bench --target "$triple"
"${objdump[@]}" -d --no-show-raw-insn --no-addresses "target/$triple/release/dispatch_bench" |
  awk -v target="$TARGET" -v arch="${1:-x86_64}" '
    /^<toi::interp::(op|slow)_/ { name = $1; gsub(/[<>:]/, "", name); sub(/^toiinterp/, "", name); n = 0; print; next }
    name && /^$/  { printf "  ; %d instructions\n\n", n; if (name == "op_nop") nop = n; name = ""; next }
    name && !/int3|udf|\.word/ { n++; print }
    END {
      printf "%s dispatch length (op_nop): %d instructions (target <= %d)\n", arch, nop, target
      exit (nop > 0 && nop <= target) ? 0 : 1
    }'
