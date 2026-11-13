"""The README's chart: Octane's scores and peak RSS, cloison's interpreter
against the other engines' and its JIT against theirs up to a like tier
(the full engines last, fainter), and an empty script's RSS. From
octane.tsv and startup.tsv; writes chart-light.svg and chart-dark.svg
(GitHub's two themes; the README picks by prefers-color-scheme).

    python3 scripts/compare/chart.py [--data DIR] [--out DIR] [--note TEXT]

DIR defaults: bench/compare and target. bench/nightly/chart.sh (not tracked)
runs it nightly and publishes the chart to the `bench` branch, which the
README reads.
"""
import argparse, collections, math, os
from html import escape

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BENCHES = ("richards deltablue crypto raytrace earley-boyer regexp splay navier-stokes "
           "pdfjs mandreel gbemu code-load box2d zlib typescript").split()
# (Each mode's own steps: cloison the accent; the engines it is set
# against one neutral; the full engines, for context, a fainter one.)
THEMES = {
    "light": dict(me="#2a78d6", peer="#86847d", context="#c4c2bb", text="#1f2328", muted="#59636e", rule="#d1d9e0"),
    "dark": dict(me="#3987e5", peer="#7a7872", context="#47463f", text="#f0f6fc", muted="#9198a1", rule="#3d444d"),
}
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"
INTERPRETERS = [("cloison-nojit", "cloison", "me"), ("v8-jitless", "V8 Ignition", "peer"),
                ("jsc-nojit", "JSC LLInt", "peer"), ("sm-nojit", "SpiderMonkey", "peer"), ("quickjs", "QuickJS", "peer")]
JITS = [("cloison", "cloison", "me"), ("v8-sparkplug", "V8 Sparkplug", "peer"), ("v8-maglev", "V8 Maglev", "peer"),
        ("jsc-baseline", "JSC Baseline", "peer"), ("jsc-dfg", "JSC DFG", "peer"),
        ("sm-baseline", "SpiderMonkey Baseline", "peer"), ("v8", "V8, all tiers", "context"),
        ("jsc", "JSC, all tiers", "context"), ("sm", "SpiderMonkey, all tiers", "context")]
STARTUP = [("cloison", "cloison", "me"), ("v8", "V8", "peer"), ("jsc", "JavaScriptCore", "peer"),
           ("sm", "SpiderMonkey", "peer"), ("quickjs", "QuickJS", "peer")]


def fmt(v):
    return f"{v:,.0f}" if v >= 100 else f"{v:.1f}"


def octane(data, rows, metric):
    d = collections.defaultdict(dict)
    for l in open(f"{data}/octane.tsv"):
        e, b, k, v = l.rstrip("\n").split("\t")
        if k == metric and v not in ("-", ""):
            d[e][b] = float(v)
    out = []
    for e, label, role in rows:
        xs = [d[e][b] for b in BENCHES if d[e].get(b, 0) > 0]
        if xs:
            out.append((label, role, math.exp(sum(map(math.log, xs)) / len(xs))))
    return out


def startup(data, rows, metric):
    t = [l.rstrip("\n").split("\t") for l in open(f"{data}/startup.tsv")]
    col = t[0].index(metric)
    v = {r[0]: float(r[col]) for r in t[1:]}
    return [(label, role, v[e]) for e, label, role in rows if e in v]


def panels(data):
    """Two columns, speed and memory: (title, subtitle, higher is better, rows)."""
    return [
        ("Octane, JITs off", "each engine's interpreter · score, higher is better", True,
         octane(data, INTERPRETERS, "score")),
        ("Memory of an empty script", "RSS in MB · lower is better", False, startup(data, STARTUP, "empty_rss_mb")),
        ("Octane, JIT tiers", "up to a tier, full engines fainter · score, higher is better", True, octane(data, JITS, "score")),
        ("Peak memory on Octane, JIT tiers", "geomean peak RSS in MB · lower is better", False,
         octane(data, JITS, "rss_mb")),
    ]


def svg(ps, theme, note):
    t = THEMES[theme]
    W, PW, GAP = 920, 440, 40  # width, a panel's, between the columns
    NAME_W, VAL_W, ROW, BAR, SEP = 172, 56, 24, 15, 10
    TOP = 50  # a panel's title and subtitle

    def height(rows):
        return TOP + len(rows) * ROW + (SEP if any(r == "context" for _, r, _ in rows) else 0) + 16

    heights = [max(height(ps[i][3]), height(ps[i + 1][3])) for i in (0, 2)]
    H = sum(heights) + 28
    o = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" '
         f'font-family="{FONT}" role="img">']
    for i, (title, sub, higher, rows) in enumerate(ps):
        x0, y0 = (i % 2) * (PW + GAP), (heights[0] if i >= 2 else 0)
        o.append(f'<text x="{x0}" y="{y0 + 18}" font-size="15" font-weight="600" fill="{t["text"]}">{escape(title)}</text>')
        o.append(f'<text x="{x0}" y="{y0 + 36}" font-size="12" fill="{t["muted"]}">{escape(sub)}</text>')
        # Best first, the full engines (context) after the rest.
        key = (lambda r: -r[2]) if higher else (lambda r: r[2])
        order = sorted([r for r in rows if r[1] != "context"], key=key) + sorted(
            [r for r in rows if r[1] == "context"], key=key)
        top = max(v for _, _, v in rows)
        span = PW - NAME_W - VAL_W
        bx = x0 + NAME_W
        y = y0 + TOP
        ys = []
        for k, (label, role, v) in enumerate(order):
            if role == "context" and k and order[k - 1][1] != "context":
                y += SEP
            ys.append(y)
            me = role == "me"
            ink = t["text"] if role != "context" else t["muted"]
            weight = ' font-weight="600"' if me else ""
            w = max(2, span * v / top)
            o.append(f'<text x="{bx - 8}" y="{y + BAR - 3}" font-size="12.5" text-anchor="end" fill="{ink}"{weight}>'
                     f'{escape(label)}</text>')
            # (Rounded at the data end only: the bar's start sits on the baseline.)
            r = min(4, w / 2)
            o.append(f'<path d="M{bx},{y} h{w - r:.1f} a{r},{r} 0 0 1 {r},{r} v{BAR - 2 * r} a{r},{r} 0 0 1 -{r},{r} '
                     f'h-{w - r:.1f} z" fill="{t[role]}"><title>{escape(label)}: {fmt(v)}</title></path>')
            o.append(f'<text x="{bx + w + 6:.1f}" y="{y + BAR - 3}" font-size="12" '
                     f'fill="{t["text"] if me else t["muted"]}"{weight}>{fmt(v)}</text>')
            y += ROW
        o.append(f'<line x1="{bx}" y1="{y0 + TOP - 4}" x2="{bx}" y2="{ys[-1] + BAR + 4}" stroke="{t["rule"]}" stroke-width="1"/>')
    o.append(f'<text x="0" y="{H - 8}" font-size="11" fill="{t["muted"]}">{escape(note)}</text>')
    o.append("</svg>")
    return "\n".join(o) + "\n"


def main():
    a = argparse.ArgumentParser()
    a.add_argument("--data", default=f"{ROOT}/bench/compare")
    a.add_argument("--out", default=f"{ROOT}/target")
    a.add_argument("--note", default="Linux x86-64; docs/bench has every benchmark, the versions and flags.")
    args = a.parse_args()
    ps = panels(args.data)
    for theme in THEMES:
        with open(f"{args.out}/chart-{theme}.svg", "w") as f:
            f.write(svg(ps, theme, args.note))
    for title, _, _, rows in ps:
        print(f"{title}: " + ", ".join(f"{label} {fmt(v)}" for label, _, v in rows))


if __name__ == "__main__":
    main()
