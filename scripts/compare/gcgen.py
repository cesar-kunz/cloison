"""M6 (generations) and the pause figures of L6, from each engine's own GC
trace of the deterministic `-run.js`. Appends to gcgen.tsv: engine, bench,
metric, value. Metrics: alloc_mb (bytes allocated), promoted_mb (young
bytes that became old), minors, majors, minor/major pause total and max (ms),
minor p50/p99 (ms).

How each engine tells it:
  cloison  CLOISON_SIZES's allocated= and promoted= (bytes allocated before
           each collection; what minor collections and a cycle's start pause
           found live), CLOISON_GC_STATS's pause of each collection.
  v8       --trace-gc-nvp: each collection's allocated and promoted (moved
           to the old generation; V8 copies a young survivor once within the
           young generation first), pause; gc "s" (scavenge) is minor.
  sm       JS_GC_PROFILE_NURSERY: each minor collection's nursery size and
           promotion rate (allocated is taken as the nursery's size: exact
           when it was full, OUT_OF_NURSERY, an upper bound otherwise);
           JS_GC_PROFILE: each major slice's pause.
  jsc      --logGC=1: bytes allocated in each cycle; an Eden collection
           (sticky mark bits, nothing moves) promotes what it leaves above
           the heap the last collection left.
  qjs      reference counting and a cycle collector: no generations.

    python3 gcgen.py engines bench...
"""
import json, os, re, statistics, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eng import run
from octane import wrapper, OCT, BENCHES
from eng import OUT


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(p / 100 * len(xs)))] if xs else 0


def measure(e, b):
    f = wrapper(e, f"{OCT}/{b}-run.js")
    minor, major, alloc, prom, nmajor = [], [], 0.0, 0.0, 0
    young = None  # bytes allocated young, where not all of alloc
    if e == "cloison":
        r = run(e, f, env={"CLOISON_SIZES": "1", "CLOISON_GC_STATS": "1"}, timeout=3600)
        for kind, ms in re.findall(r"^gc \d+ (\w+): ([\d.]+) ms", r.stderr, re.M):
            (minor if kind in ("Minor", "MinorShade") else major).append(float(ms))
            # (A concurrent cycle pauses twice, Start and Final: one major.)
            nmajor += kind in ("Full", "Final")
        g = dict(re.findall(r"(\w+)=(\d+)", re.search(r"^gc minors=.*$", r.stderr, re.M).group(0)))
        alloc, prom = int(g["allocated"]), int(g["promoted"])
    elif e == "v8":
        r = run(e, f, args=["--trace-gc-nvp"], timeout=3600)
        young, young_end = 0, 0
        for l in r.stdout.splitlines() + r.stderr.splitlines():
            if "GC: {" not in l:
                continue
            j = json.loads(l.split("GC: ", 1)[1])
            (minor if j["gc"] in ("s", "mmc") else major).append(j["pause"])
            alloc += j.get("allocated", 0)
            prom += j.get("promoted", 0)
            # (What the young generation held at the start, less the
            # survivors the last collection left there, was allocated in it.)
            young += max(0, j["start_object_size"] - j["start_old_gen_consumed_size"] - young_end)
            young_end = max(0, j["end_object_size"] - j["end_old_gen_consumed_size"])
    elif e == "sm":
        r = run(e, f, env={"JS_GC_PROFILE_NURSERY": "0", "JS_GC_PROFILE": "0"}, timeout=3600)
        major_slices = []
        for l in (r.stdout + r.stderr).splitlines():
            w = l.split()
            if len(w) < 10 or not w[1].isdigit() or not re.fullmatch(r"[\d.]+", w[3]):
                continue  # (headers, TOTALS)
            if w[0] == "MinorGC:":
                rate, old_kb, total_us = float(w[5].rstrip("%")), int(w[6]), float(w[9])
                young = (young or 0) + old_kb * 1024
                prom += rate / 100 * old_kb * 1024
                minor.append(total_us / 1000)
            elif w[0] == "MajorGC:":
                # States `a -> b`, flags (FSNR) if any, SizeKB MllcKB Zs Cs Rs Budget total (ms).
                i = w.index("->")
                rest = [t for t in w[i + 2:] if not re.fullmatch(r"[A-Z]", t)]
                major_slices.append(float(rest[6]))
                if w[i + 1] == "0":
                    major.append(0.0)
        # (A major collection's pause: its slices; majors counts the collections.)
        nmajor = len(major)
        major = major_slices
    elif e == "jsc":
        r = run(e, f, args=["--logGC=1"], timeout=3600)
        out = r.stdout + r.stderr
        last_after = None
        # One collection: from its START to its END.
        pending_alloc = 0
        for block in re.split(r"(?=\[GC<[^>]*>: START )", out):
            a = re.findall(r"bytes allocated this cycle: (\d+)", block)
            if not block.startswith("[GC<"):
                pending_alloc += sum(map(int, a))
                continue
            kind = re.search(r"=> (\w+)Collection", block).group(1)
            after = re.search(r"=> (\d+)kb, p=", block)
            pause = sum(map(float, re.findall(r"\bp=([\d.]+)ms", block)))
            alloc += pending_alloc
            pending_alloc = sum(map(int, a))
            (minor if kind == "Eden" else major).append(pause)
            if after:
                after = int(after.group(1)) * 1024
                if kind == "Eden" and last_after is not None:
                    prom += max(0, after - last_after)
                last_after = after
    ok = any(l.strip() in ("1001", "1002") for l in r.stdout.splitlines())
    if young is None:
        young = alloc  # (cloison, JSC: every cell starts young)
    if e == "sm":
        alloc = float("nan")  # (tenured allocation not in its trace)
    return ok, {
        "alloc_mb": alloc / 2**20, "young_mb": young / 2**20,
        "young_pct": 100 * young / alloc if alloc else 0,
        "promoted_mb": prom / 2**20,
        "survival_pct": 100 * prom / young if young else 0,
        "minors": len(minor), "majors": nmajor if e in ("sm", "cloison") else len(major),
        "minor_ms": sum(minor), "minor_max_ms": max(minor, default=0),
        "minor_p50_ms": pct(minor, 50), "minor_p99_ms": pct(minor, 99),
        "major_ms": sum(major), "major_max_ms": max(major, default=0),
    }


if __name__ == "__main__":
    engines = sys.argv[1].split(",")
    out = open(f"{OUT}/gcgen.tsv", "a")
    for b in sys.argv[2:] or BENCHES:
        for e in engines:
            ok, m = measure(e, b)
            if not ok:
                print(f"{e} {b}: FAILED", file=sys.stderr, flush=True)
                continue
            for k, v in m.items():
                out.write(f"{e}\t{b}\t{k}\t{v:.3f}\n")
            out.flush()
            print(e, b, " ".join(f"{k}={v:.1f}" for k, v in m.items()), file=sys.stderr, flush=True)
