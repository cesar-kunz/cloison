#!/usr/bin/env bash
# Run a command (a benchmark, or `perf stat` around one) pinned to one CPU
# (`taskset`): the idlest core whose SMT sibling is idle too, so the run
# neither migrates nor shares a core. Children inherit the pinning.
#
#   scripts/quiet.sh perf stat -e cycles:u target/release/toi bench/micro/44-array-scan-filter.js
#   QUIET_CPU=7 scripts/quiet.sh …   # pin to CPU 7
#   scripts/quiet.sh --cpu           # print the CPU it would pick
#   scripts/quiet.sh --core          # print that CPU and its siblings (`a,b`)
#
# For measures of time (scripts/vs-v8.sh). Without root the frequency
# governor (schedutil) and boost stay as they are, and time follows the
# clock: unpinned runs land on cold cores. Measured on this host
# (2026-10-08, 15 runs of 20, 40, 44, 69, 70): pinning cut the spread of
# CPU time from 20-33% (sd) to 2-4%. Cycles barely depend on the clock and
# pinning doesn't help them: on this shared machine another process may
# land on the pinned CPU, which a pinned run can't leave (scripts/perf.sh
# was 4-8% slower and noisier pinned; it interleaves builds instead,
# `--ab`). Also tried, and left out:
# address randomization off (`setarch -R`) changed nothing; transparent
# huge pages off for the process (`prctl(PR_SET_THP_DISABLE)`) cost 20-50%
# more cycles, as the heap asks for huge pages (`MADV_HUGEPAGE`), so it
# would measure something else. A run the kernel can't give huge pages is
# the remaining outlier (69: 17M cycles instead of 10M): take medians.
set -euo pipefail

# The CPU whose core (it and its siblings) had the most idle time over
# 0.3 s, and its siblings; not CPU 0's core (interrupts, housekeeping).
pick() {
  python3 - "$1" <<'PY'
import sys, time
def snap():
    r = {}
    for l in open("/proc/stat"):
        if l.startswith("cpu") and l[3].isdigit():
            f = l.split(); v = list(map(int, f[1:]))
            r[int(f[0][3:])] = (v[3] + v[4], sum(v))
    return r
a = snap(); time.sleep(0.3); b = snap()
busy = {c: 1 - (b[c][0] - a[c][0]) / max(1, b[c][1] - a[c][1]) for c in a}
def sibs(c):
    s = open(f"/sys/devices/system/cpu/cpu{c}/topology/thread_siblings_list").read().strip()
    out = []
    for p in s.split(","):
        lo, _, hi = p.partition("-")
        out += range(int(lo), int(hi or lo) + 1)
    return out
best = min((c for c in busy if c not in sibs(0)), key=lambda c: sum(busy[s] for s in sibs(c)))
print(best if sys.argv[1] == "cpu" else ",".join(map(str, sibs(best))))
PY
}

case ${1:-} in
--cpu) echo "${QUIET_CPU:-$(pick cpu)}"; exit ;;
--core) pick core; exit ;;
esac
exec taskset -c "${QUIET_CPU:-$(pick cpu)}" "$@"
