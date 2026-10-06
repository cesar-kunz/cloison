#!/usr/bin/env bash
# Build release and dump every opcode handler's machine code, with instruction
# counts (int3 padding excluded). `toi_op_nop` is pure dispatch, so its count
# is the dispatch length. Exits non-zero if it exceeds TARGET (default 5).
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET=${TARGET:-5}
cargo build --release --quiet --bin dispatch_bench
objdump -d --no-show-raw-insn -M intel --no-addresses target/release/dispatch_bench |
  awk -v target="$TARGET" '
    /^<toi_op_/   { name = $1; gsub(/[<>:]/, "", name); n = 0; print; next }
    name && /^$/  { printf "  ; %d instructions\n\n", n; if (name == "toi_op_nop") nop = n; name = ""; next }
    name && !/int3/ { n++; print }
    END {
      printf "dispatch length (toi_op_nop): %d instructions (target <= %d)\n", nop, target
      exit (nop > 0 && nop <= target) ? 0 : 1
    }'
