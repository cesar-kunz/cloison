"""Markdown tables from startup.tsv, octane.tsv, gcgen.tsv: per benchmark,
then a geomean per engine with its ratio to the best and its rank."""
import collections, math, os, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eng import OUT
BENCHES = ("richards deltablue crypto raytrace earley-boyer regexp splay navier-stokes "
           "pdfjs mandreel gbemu code-load box2d zlib typescript").split()
JIT = ["cloison", "v8", "jsc", "sm"]
NOJIT = ["cloison-nojit", "v8-jitless", "jsc-nojit", "sm-nojit", "qjs"]


def load(name):
    d = collections.defaultdict(dict)
    if os.path.exists(f"{OUT}/{name}"):
        for l in open(f"{OUT}/{name}"):
            e, b, k, v = l.rstrip("\n").split("\t")
            try:
                d[(e, b)][k] = float(v)
            except ValueError:
                pass
    return d


def geomean(xs):
    xs = [x for x in xs if x and x > 0 and not math.isnan(x)]
    return math.exp(sum(map(math.log, xs)) / len(xs)) if xs else None


def fmt(v):
    if v is None or (isinstance(v, float) and math.isnan(v)):
        return "—"
    if abs(v) >= 1000:
        return f"{v:,.0f}"
    if abs(v) >= 100:
        return f"{v:.0f}"
    if abs(v) >= 10:
        return f"{v:.1f}"
    return f"{v:.2f}"


def table(title, d, metric, engines, higher_better, scale=1.0, benches=BENCHES, agg="geomean"):
    """Per-benchmark values, the geomean, its ratio to the best, the rank."""
    print(f"\n### {title}\n")
    print("| benchmark | " + " | ".join(engines) + " |")
    print("|---|" + "---:|" * len(engines))
    complete = [b for b in benches if all(metric in d.get((e, b), {}) for e in engines)]
    for b in benches:
        print(f"| {b} | " + " | ".join(fmt(d.get((e, b), {}).get(metric, float('nan')) * scale) for e in engines) + " |")
    # (Counts and pauses can be 0: summed over the benchmarks, not a geomean.)
    f = {"geomean": geomean, "sum": sum, "max": max}[agg]
    g = {e: f([d[(e, b)][metric] * scale for b in complete]) for e in engines}
    print(f"| **{agg}** ({len(complete)} complete) | " + " | ".join(f"**{fmt(g[e])}**" for e in engines) + " |")
    valid = {e: v for e, v in g.items() if v is not None and not math.isnan(v) and (v > 0 or agg != "geomean")}
    if valid:
        best = max(valid.values()) if higher_better else min(valid.values())
        order = sorted(valid, key=lambda e: -valid[e] if higher_better else valid[e])
        print("| ratio to best | " + " | ".join(fmt(valid[e] / best) if e in valid and best else "—" for e in engines) + " |")
        print("| rank | " + " | ".join(str(order.index(e) + 1) if e in valid else "—" for e in engines) + " |")
    return g


if __name__ == "__main__":
    o = load("octane.tsv")
    gc = load("gcgen.tsv")
    print("## T4: Octane score (higher is better)")
    table("JIT engines", o, "score", JIT, True)
    table("Interpreters", o, "score", NOJIT, True)
    print("\n## T5: fixed work (the deterministic run), lower is better")
    table("Instructions (G), JIT engines", o, "instructions", JIT, False, 1e-9)
    table("Instructions (G), interpreters", o, "instructions", NOJIT, False, 1e-9)
    table("CPU time (s), JIT engines", o, "cpu_ms", JIT, False, 1e-3)
    table("Peak RSS (MB), JIT engines", o, "rss_mb", JIT, False)
    table("Peak RSS (MB), interpreters", o, "rss_mb", NOJIT, False)
    if gc:
        gce = ["cloison", "v8", "jsc", "sm"]
        print("\n## M6: generations")
        table("Bytes allocated (MB)", gc, "alloc_mb", gce, False)
        table("Allocated young (%)", gc, "young_pct", gce, True)
        table("Young survival: promoted / allocated young (%)", gc, "survival_pct", gce, False)
        table("Minor collections", gc, "minors", gce, False, agg="sum")
        table("Major collections", gc, "majors", gce, False, agg="sum")
        table("Minor pauses, total (ms)", gc, "minor_ms", gce, False, agg="sum")
        table("Major pauses, total (ms)", gc, "major_ms", gce, False, agg="sum")
        table("Longest pause, minor (ms)", gc, "minor_max_ms", gce, False, agg="max")
        table("Longest pause, major (ms)", gc, "major_max_ms", gce, False, agg="max")
