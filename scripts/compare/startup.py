"""L1 (startup) and M1 (baseline RSS): an empty script and a hello world."""
import os, re, statistics, sys
sys.path.insert(0, os.path.dirname(__file__))
from eng import ENGINES, run, OUT, TMP

open(f"{TMP}/empty.js", "w").write("0\n")
open(f"{TMP}/hello.js", "w").write('print("hello")\n')
N = 50
out = open(f"{OUT}/startup.tsv", "w")
out.write("engine\tempty_wall_ms\tempty_cpu_ms\tempty_Minstr\tempty_rss_mb\thello_wall_ms\thello_cpu_ms\thello_Minstr\thello_rss_mb\n")
for name in ENGINES:
    row = [name]
    for s in ("empty", "hello"):
        f = f"{TMP}/{s}.js"
        r = run(name, f, wrap=["perf", "stat", "-r", str(N), "-x,", "-e", "instructions,task-clock"])
        instr = int(re.search(r"^(\d+),,instructions", r.stderr, re.M).group(1))
        cpu = float(re.search(r"^([\d.]+),msec,task-clock", r.stderr, re.M).group(1))
        r = run(name, f, wrap=["perf", "stat", "-r", str(N)])
        wall = float(re.search(r"([\d.]+) \+- [\d.]+ seconds time elapsed", r.stderr).group(1)) * 1000
        rss = statistics.median(int(run(name, f, wrap=["/usr/bin/time", "-f", "%M"]).stderr.split()[-1]) for _ in range(9))
        row += [f"{wall:.2f}", f"{cpu:.2f}", f"{instr / 1e6:.2f}", f"{rss / 1024:.1f}"]
    print("\t".join(row), flush=True)
    out.write("\t".join(row) + "\n")
