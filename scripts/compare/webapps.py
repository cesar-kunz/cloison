"""W1: real web applications' bundles (TypeScript's compiler, pdf.js,
Excalidraw, ECharts, ...), each engine running each app once: the process's
wall time and peak RSS, and whether the app did the reference's work.
Appends rows to webapps.tsv (WEBAPPS_TSV): engine, app, metric, value.

    WEBAPPS=<dir> python3 webapps.py [engines,...] [app ...]

<dir> holds the bundles, `bundles/<app>.js`, each printing a JSON report
between `@@REALAPPS-JSON@@` and `@@END@@`, and `expected/work.json`, each
kernel's reference work (calls and hash). cloison runs the `intl` build when
there is one (target/intl/release/cloison: TypeScript's completions need
`Intl.Collator`); ARK runs the bundles as `es2abc` compiles them.
"""
import json, os, re, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eng import ENGINES, C, ROOT, OUT, cmd, abc

DIR = os.path.expanduser(os.environ.get("WEBAPPS", ""))
BUNDLES = os.path.join(DIR, "bundles")
# The run order.
APPS = ["excalidraw", "pdfjs", "typescript", "jsnes", "sass", "sheetjs", "echarts", "mathjax",
        "snap", "cyberchef", "beepbox", "pcjs", "univer", "siyuan", "tiddlywiki"]
MARK = re.compile(r"@@REALAPPS-JSON@@\n(.*?)\n@@END@@", re.S)


def check(app, output, expected):
    """ok, work (it did other work than the reference) or fail, and why."""
    m = MARK.search(output)
    if not m:
        return "fail", "no report"
    rep = json.loads(m.group(1))
    errs = rep["errors"]["count"] + (rep.get("fnTableMisses") or 0) + rep.get("missCount", 0)
    if errs:
        first = ((rep["errors"].get("samples") or [""])[0].splitlines() or [""])[0][:200]
        return "fail", f"{errs} app error(s)/miss(es): {first}"
    ref = expected.get(app)
    if ref:
        got = {b["label"]: [b["work"]["calls"], b["work"]["hash"]] for b in rep["benchmarks"]}
        diff = sorted(k for k in ref if got.get(k) != ref[k])
        if diff:
            return "work", "did other work than the reference in: " + ", ".join(diff)
    return "ok", f"{len(rep['benchmarks'])} kernels"


def tail(text, n=4):
    """The engine's last lines, led by its first error line."""
    lines = [l.strip() for l in text.splitlines() if l.strip()]
    err = next((l for l in lines if re.match(r"FAILED|Uncaught|.*(Error|Exception)\b", l) and not l.startswith("at ")), None)
    return " | ".join(([err] if err else []) + [l for l in lines[-n:] if l != err])[:600]


INTL = os.path.join(ROOT, "target", "intl", "release", "cloison")
TIMEOUT = int(os.environ.get("WEBAPP_TIMEOUT", "1800"))


def argv_env(engine):
    argv, env = cmd(engine)
    if engine.startswith("cloison") and os.path.exists(INTL):
        argv = [INTL if a == C else a for a in argv]
    return argv, env


if __name__ == "__main__":
    engines = sys.argv[1].split(",") if len(sys.argv) > 1 and sys.argv[1] != "all" else list(ENGINES)
    if not os.path.isdir(BUNDLES):
        raise SystemExit("set WEBAPPS to the directory holding bundles/ and expected/work.json")
    apps = sys.argv[2:] or APPS
    expected = json.load(open(os.path.join(DIR, "expected", "work.json")))
    out = open(os.environ.get("WEBAPPS_TSV", f"{OUT}/webapps.tsv"), "a")
    logs = os.path.join(ROOT, "target", "compare", "webapps")
    os.makedirs(logs, exist_ok=True)

    def emit(e, a, k, v):
        out.write(f"{e}\t{a}\t{k}\t{v}\n")
        out.flush()

    for e in engines:
        argv, env = argv_env(e)
        for a in apps:
            js = os.path.join(BUNDLES, a + ".js")
            script = abc(js) if e.startswith("ark") else js
            if script is None:
                emit(e, a, "status", "fail")
                emit(e, a, "why", "es2abc failed")
                continue
            log = f"{logs}/{e}-{a}.out"
            t0 = time.perf_counter()
            try:
                with open(log, "w") as f:
                    p = subprocess.run(["/usr/bin/time", "-f", "TIME %e %M", "-o", log + ".time"] + argv + [script],
                                       stdout=f, stderr=subprocess.STDOUT, env=env, timeout=TIMEOUT)
                rc = p.returncode
            except subprocess.TimeoutExpired:
                rc = None
            wall = time.perf_counter() - t0
            text = open(log, errors="replace").read()
            if rc is None:
                st, why = "fail", f"timed out after {TIMEOUT} s"
            elif rc != 0:
                st, why = "fail", f"exit status {rc}: {tail(text)}"
            else:
                st, why = check(a, text, expected)
                if st == "fail" and why == "no report":
                    why += ": " + tail(text)
            emit(e, a, "status", st)
            if st != "ok":
                emit(e, a, "why", why.replace("\t", " ").replace("\n", " ")[:300])
            m = re.search(r"TIME ([\d.]+) (\d+)", open(log + ".time").read()) if rc is not None else None
            if m:
                emit(e, a, "wall_s", m.group(1))
                emit(e, a, "rss_mb", round(int(m.group(2)) / 1024, 1))
            print(f"{e} {a}: {st} {wall:.1f}s {int(m.group(2)) // 1024 if m else '-'}MB {'' if st == 'ok' else why[:120]}",
                  file=sys.stderr, flush=True)
