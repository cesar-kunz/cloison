#!/usr/bin/env bash
# Profile a bytecode listing and show where compiled code spends its cycles,
# per stencil piece, in layout order (the JIT names each piece in a perf map
# with `CLOISON_PERF_MAP`). Without precise sampling a stall is often counted on
# the piece after it.
#
#   scripts/jit-profile.sh bench/bytecode/21-monomorphic-get.asm
#   MIN=0.5 FREQ=20000 scripts/jit-profile.sh file.asm   # pieces >= 0.5%, sampling rate
#
# Short runs give few samples (perf throttles): repeat the work in the
# listing (e.g. call `main` many times) for a steady-state profile.
set -euo pipefail
cd "$(dirname "$0")/.."
listing=${1:?listing}
cargo build --release --quiet --bin cloison
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
CLOISON_PERF_MAP=1 perf record -q -e cycles:u -F "${FREQ:-20000}" -o "$dir/perf.data" \
  target/release/cloison "$listing" >/dev/null
# The map is named after cloison's pid: the newest one.
map=$(ls -t /tmp/perf-*.map 2>/dev/null | head -1)
[[ -n $map ]] || { echo "no perf map written" >&2; exit 1; }
perf script -i "$dir/perf.data" -F ip 2>/dev/null >"$dir/ips"
python3 - "$map" "$dir/ips" "${MIN:-1.0}" <<'EOF'
import bisect, collections, sys
mapf, ipf, low = sys.argv[1], sys.argv[2], float(sys.argv[3])
ents = sorted((int(a, 16), int(n, 16), name.strip())
              for a, n, name in (l.split(' ', 2) for l in open(mapf)))
starts = [e[0] for e in ents]
cnt, total = collections.Counter(), 0
for l in open(ipf):
    ip = int(l, 16)
    total += 1
    k = bisect.bisect_right(starts, ip) - 1
    if k >= 0 and ip < ents[k][0] + ents[k][1]:
        cnt[k] += 1
jit = sum(cnt.values())
print(f"{total} samples, {100 * jit / max(total, 1):.0f}% in compiled code; pieces by share of those:")
for k, (a, n, name) in enumerate(ents):
    share = 100 * cnt[k] / max(jit, 1)
    if share >= low:
        print(f"  {a:x} {name:40} {share:5.1f}%")
EOF
rm -f "$map"
