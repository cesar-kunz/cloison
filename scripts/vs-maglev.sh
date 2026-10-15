#!/usr/bin/env bash
# Compare the machine code toi's JIT generates for each script in
# bench/micro/ with the code V8's Maglev generates for it, function by
# function, to find where toi's code is larger than it needs to be.
#
#   scripts/vs-maglev.sh                     # every file; table in perf-history/vs-maglev-<commit>.md
#   scripts/vs-maglev.sh bench/micro/1[0-3]*.js   # only these
#   scripts/vs-maglev.sh --asm FILE FN       # both disassemblies of one function's last compile
#
# toi: `TOI_JIT_LOG` lines, each compile's hot code, out-of-line code
# (edge stubs, slow-path helpers, runtime-call wrappers) and exits (stubs
# rebuilding the interpreter's frame). Maglev: V8 capped at Maglev
# (`--max-opt=2`), the size of each Maglev code object from V8's perf map
# (`--perf-basic-prof`; its instructions, deferred code and deopt exits
# included, its metadata not),
# compiling on the main thread (`--single-threaded`: with background
# compiles, which code gets installed when varies from run to run).
#
# "toi" is hot plus out-of-line code: what Maglev's size counts (its
# deferred code). Exits are apart: Maglev's are a 4-byte call each, the
# frame they rebuild described in metadata.
#
# Per function, both engines' *final* code: the last compile of each
# entry, the function's own (toi `@0`, Maglev `+`) and on-stack
# replacement (toi `@pc`, Maglev `o+`), summed. Functions are matched by
# name: V8 names `X.prototype.m = function` `X.m`, toi `m`, so V8's names
# are cut after their last `.`, and same-named functions are summed in
# both. Both inline small callees: code a function inlines is in its
# size, and a function that is only ever inlined has none of its own (one
# engine may compile it alone and the other not: "same fns" compares just
# the functions both compiled).
# "all" sums every compile, recompiles after deopts included.
#
# Maglev's sizes don't change with toi, so they're cached in
# perf-history/maglev-sizes.tsv by Node version and the file's contents
# (`git hash-object`); MAGLEV=1 measures them again.
#
# Node: $NODE, else the Node 26 scripts/tiers.sh downloads.
set -euo pipefail
cd "$(dirname "$0")/.."
NODE=${NODE:-$(ls -d ~/.local/share/toi-v8/node-*/bin/node 2>/dev/null | tail -1)}
[[ -x $NODE ]] || { echo "no Node 26: run scripts/tiers.sh once, or set NODE" >&2; exit 1; }
cargo build --release --quiet --bin toi

if [[ ${1:-} == --asm ]]; then
  file=${2:?file}; fn=${3:?function}
  dump=$(mktemp); trap 'rm -f "$dump"' EXIT
  TOI_JIT_DUMP=$dump TOI_JIT_DUMP_FN=$fn target/release/toi "$file" >/dev/null
  if [[ ! -s $dump ]]; then
    echo "=== toi: $fn not compiled on its own (inlined into its callers, or never hot)"
  else
    echo "=== toi: $fn, last compile, hot code ($(stat -c %s "$dump") bytes)"
  fi
  [[ -s $dump ]] && objdump -D -b binary -m i386:x86-64 -M intel --no-show-raw-insn "$dump" | sed -n '/<.data>:/,$p' | tail -n +2
  echo
  # The last Maglev code object named `fn` or `X.fn`.
  "$NODE" --max-opt=2 --single-threaded --print-maglev-code "$file" 2>&1 | awk -v fn="$fn" '
    /^--- Disassembly: ---/ { inblk = 0 }
    /^kind = MAGLEV/ { kind = 1; next }
    kind && /^name = / { n = substr($0, 8); kind = 0
      if (n == fn || substr(n, length(n) - length(fn)) == "." fn) { inblk = 1; buf = "=== Maglev: " n "\n" } next }
    inblk && /^Instructions/ { ins = 1 }
    inblk && ins { buf = buf $0 "\n"; if ($0 ~ /^$/ && lines++ > 0) { last = buf; inblk = 0; ins = 0; lines = 0 } }
    END { printf "%s", last }'
  exit
fi

commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils build.rs .cargo || commit="$commit+"
mkdir -p perf-history
files=("$@")
[[ ${#files[@]} -gt 0 ]] || files=(bench/micro/*.js)
python3 - "$NODE" "$commit" "${MAGLEV:-0}" "${files[@]}" <<'EOF'
import collections, math, os, re, subprocess, sys, tempfile

node, commit, remeasure, *files = sys.argv[1:]
remeasure = remeasure == "1"
v8 = subprocess.run([node, "-p", "process.versions.v8"], capture_output=True, text=True).stdout.strip()
tmp = tempfile.mkdtemp()
cache_path = "perf-history/maglev-sizes.tsv"

def final(compiles):
    """Per function, the last compile of each entry kind, summed; and every compile summed."""
    last, every = {}, collections.Counter()
    for name, kind, size in compiles:
        last[(name, kind)] = size
        every[name] += size
    fin = collections.Counter()
    for (name, _), size in last.items():
        fin[name] += size
    return fin, every

def toi(f):
    r = subprocess.run(["target/release/toi", f], capture_output=True, text=True,
                       env={**os.environ, "TOI_JIT_LOG": "1"}, timeout=600)
    want = re.search(r"^// expect: (.*)$", open(f).read(), re.M)
    ok = want is not None and r.stdout.strip().splitlines()[-1:] == [want.group(1).strip()]
    hot, code, exits = [], [], []
    for name, pc, h, o, e, _ in re.findall(
            r"^jit (\S*) @(\d+) \((\d+) hot, (\d+) out-of-line, (\d+) exits, (\d+) bytes\)", r.stderr, re.M):
        kind = "entry" if pc == "0" else "osr"
        hot.append((name, kind, int(h)))
        code.append((name, kind, int(h) + int(o)))
        exits.append((name, kind, int(e)))
    return ok, final(hot), final(code), final(exits)

def maglev(f):
    out = os.path.join(tmp, "perf")
    os.makedirs(out, exist_ok=True)
    src = ('const fs=require("fs"),vm=require("vm"),f=process.argv[1];'
           'console.log(String(vm.runInThisContext(fs.readFileSync(f,"utf8"),{filename:f})))')
    r = subprocess.run([node, "--max-opt=2", "--single-threaded", "--logfile=/dev/null", "--perf-basic-prof", "--perf-basic-prof-only-functions",
                        f"--perf-basic-prof-path={out}", "-e", src, os.path.abspath(f)],
                       capture_output=True, text=True, timeout=600)
    compiles, ids = [], collections.defaultdict(set)
    for m in os.listdir(out):
        path = os.path.join(out, m)
        for line in open(path):
            # `addr size JS:<tier>name file:line:col`: `+` Maglev, `o+` its
            # on-stack replacement, `~` Ignition, `^` Sparkplug.
            p = line.split(" ", 2)
            if len(p) == 3 and os.path.abspath(f) in p[2]:
                mm = re.match(r"JS:(o?)([~^+*])'?(.*) (\S+)$", p[2].strip())
                if mm:
                    # (A script's top level has no name: toi's `<script>`.)
                    name = mm.group(3).rsplit(".", 1)[-1] or "<script>"
                    ids[name].add(mm.group(4))
                    if mm.group(2) == "+":
                        compiles.append((name, "osr" if mm.group(1) else "entry", int(p[1], 16)))
        os.remove(path)
    # Names V8 saw on more than one function (toi's log can't tell them apart).
    compiles += [(name, "dup", 0) for name, where in ids.items() if len(where) > 1]
    return compiles

def blob(f):
    return subprocess.run(["git", "hash-object", f], capture_output=True, text=True).stdout.strip()

cache = collections.defaultdict(list)
if os.path.exists(cache_path):
    for l in open(cache_path).read().splitlines()[1:]:
        ver, h, name, kind, size = l.split("\t")
        cache[(ver, h)].append((name, kind, int(size)))
rows, details, added = [], {}, False
for n, f in enumerate(files, 1):
    print(f"\r{n}/{len(files)} {os.path.basename(f):40}", end="", file=sys.stderr, flush=True)
    b = os.path.basename(f)[:-3]
    key = (v8, blob(f))
    if remeasure or key not in cache:
        cache[key] = maglev(f) or [("-", "none", 0)]
        added = True
    dups = {c[0] for c in cache[key] if c[1] == "dup"}
    mfin, mall = final([c for c in cache[key] if c[1] in ("entry", "osr")])
    ok, (hfin, _), (cfin, call), (efin, _) = toi(f)
    names = sorted(set(mfin) | set(cfin), key=lambda k: -max(cfin[k], mfin[k]))
    both = [k for k in names if k in mfin and k in cfin and k not in dups]
    s = lambda c, ks: sum(c[k] for k in ks)
    rows.append(dict(bench=b, ok=ok,
        toi_hot=sum(hfin.values()), toi_code=sum(cfin.values()), toi_exits=sum(efin.values()),
        maglev=sum(mfin.values()), both_code=s(cfin, both), both_maglev=s(mfin, both),
        ambiguous=len(dups & (set(mfin) | set(cfin))), toi_all=sum(call.values()), maglev_all=sum(mall.values())))
    details[b] = [(k + (" (several)" if k in dups else ""), hfin.get(k), cfin.get(k), efin.get(k), mfin.get(k))
                  for k in names]
print(file=sys.stderr)
if added:
    with open(cache_path, "w") as out:
        out.write("v8\tblob\tfunction\tkind\tbytes\n")
        for (ver, h), cs in sorted(cache.items()):
            for name, kind, size in cs:
                out.write(f"{ver}\t{h}\t{name}\t{kind}\t{size}\n")

ratio = lambda a, b: f"{a / b:.2f}" if a and b else "-"
num = lambda v: "-" if not v else f"{v:,}"
lines = [f"toi {commit} against Maglev (V8 {v8}, `--max-opt=2`): final machine code in bytes, every "
         "function's last compile of each entry summed (`scripts/vs-maglev.sh`). toi: hot plus out-of-line code "
         "(edge stubs, slow-path helpers, runtime-call wrappers), what Maglev's size counts (its deferred code); "
         "toi hot: just the hot part; exits apart (Maglev's: a 4-byte call each, the frame in metadata). "
         "Ratios toi/Maglev (above 1: toi's is larger). Code a function inlines counts in it, and Maglev often "
         "also compiles on their own functions toi only inlines (see the per-function tables). \"same fns\": "
         "toi/M over just the functions both compiled on their own, by name, but for names several functions "
         "share (\"dup\": toi's log names functions without their class). \"all\": every compile's toi code, "
         "recompiles included.", "",
         "| benchmark | toi | toi hot | exits | Maglev | toi/M | hot/M | +exits/M | same fns toi/M | dup | toi all | Maglev all |",
         "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
keys = ["bench", "ok", "toi_code", "toi_hot", "toi_exits", "maglev", "both_code", "both_maglev", "ambiguous",
        "toi_all", "maglev_all"]
tsv = ["\t".join(keys)]
for r in rows:
    flag = "" if r["ok"] else " (wrong result)"
    lines.append(f"| {r['bench']}{flag} | {num(r['toi_code'])} | {num(r['toi_hot'])} | {num(r['toi_exits'])} | "
                 f"{num(r['maglev'])} | {ratio(r['toi_code'], r['maglev'])} | {ratio(r['toi_hot'], r['maglev'])} | "
                 f"{ratio(r['toi_code'] + r['toi_exits'], r['maglev'])} | {ratio(r['both_code'], r['both_maglev'])} | "
                 f"{num(r['ambiguous'])} | {num(r['toi_all'])} | {num(r['maglev_all'])} |")
    tsv.append("\t".join(str(r[k]) for k in keys))
for r in rows:
    r["toi_code_exits"] = r["toi_code"] + r["toi_exits"]
tot = {k: sum(r[k] for r in rows if r["ok"]) for k in ["toi_code", "toi_hot", "toi_exits", "maglev"]}
matched = [r for r in rows if r["ok"] and r["maglev"] and r["toi_code"]]
gm = lambda key: math.exp(sum(math.log(r[key] / r["maglev"]) for r in matched) / len(matched)) if matched else 0
lines += ["", f"Sum (right results): toi {tot['toi_code']:,} (hot {tot['toi_hot']:,}), exits {tot['toi_exits']:,}, "
          f"Maglev {tot['maglev']:,}; geomean toi/M {gm('toi_code'):.2f}, hot/M {gm('toi_hot'):.2f}, "
          f"+exits/M {gm('toi_code_exits'):.2f} over the {len(matched)} files both compiled.", ""]
lines += ["**Per function** (final code, bytes; largest first)", ""]
for b, ds in details.items():
    lines += [f"<details><summary>{b}</summary>", "", "| function | toi | toi hot | exits | Maglev | toi/M |",
              "|---|---:|---:|---:|---:|---:|"]
    lines += [f"| {k} | {num(c)} | {num(h)} | {num(e)} | {num(m)} | {ratio(c, m)} |" for k, h, c, e, m in ds]
    lines += ["", "</details>", ""]
out = f"perf-history/vs-maglev-{commit}"
open(out + ".md", "w").write("\n".join(lines) + "\n")
open(out + ".tsv", "w").write("\n".join(tsv) + "\n")
# The terminal gets the summary table, by toi/Maglev, largest first.
print(f"{'benchmark':40} {'toi':>8} {'toi hot':>8} {'exits':>8} {'Maglev':>8} {'toi/M':>6} {'hot/M':>6} {'same fns':>8}")
for r in sorted(rows, key=lambda r: -(r["toi_code"] / r["maglev"]) if r["maglev"] else 0):
    print(f"{r['bench'] + ('' if r['ok'] else ' !'):40} {r['toi_code']:8,} {r['toi_hot']:8,} {r['toi_exits']:8,} "
          f"{r['maglev']:8,} {ratio(r['toi_code'], r['maglev']):>6} {ratio(r['toi_hot'], r['maglev']):>6} "
          f"{ratio(r['both_code'], r['both_maglev']):>8}")
print(next(l for l in lines if l.startswith("Sum")))
print(f"tables in {out}.md and .tsv")
EOF
