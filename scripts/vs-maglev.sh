#!/usr/bin/env bash
# Compare the machine code cloison's JIT generates for each script in
# bench/micro/ with the code V8's Maglev generates for it, function by
# function, to find where cloison's code is larger than it needs to be.
#
#   scripts/vs-maglev.sh                     # every file; table in perf-history/vs-maglev-<commit>.md
#   scripts/vs-maglev.sh bench/micro/1[0-3]*.js   # only these
#   scripts/vs-maglev.sh --asm FILE FN [V8FN]   # both disassemblies of one function's last compile
#                                            # (FN: `name` or `name:line:col`; V8FN: Maglev's, e.g. `Steps.add`)
#   scripts/vs-maglev.sh --pieces [--record] [FILES]   # cloison's final code per stencil and per task
#
# cloison: `CLOISON_JIT_LOG` lines, each compile's hot code, out-of-line code
# (edge stubs, slow-path helpers, runtime-call wrappers) and exits (stubs
# rebuilding the interpreter's frame). Maglev: V8 capped at Maglev
# (`--max-opt=2`), the size of each Maglev code object from V8's perf map
# (`--perf-basic-prof`; its instructions, deferred code and deopt exits
# included, its metadata not),
# compiling on the main thread (`--single-threaded`: with background
# compiles, which code gets installed when varies from run to run).
#
# "cloison" is hot plus out-of-line code: what Maglev's size counts (its
# deferred code). Exits are apart: Maglev's are a 4-byte call each, the
# frame they rebuild described in metadata.
#
# Per function, both engines' *final* code: the last compile of each
# entry, the function's own (cloison `@0`, Maglev `+`) and on-stack
# replacement (cloison `@pc`, Maglev `o+`), summed. Functions are matched by
# name and source position, `name:line:col` (both place a function at its
# parameter list's `(`; V8 names `X.prototype.m = function` `X.m`, cloison
# `m`, so V8's names are cut after their last `.`). Both inline small callees: code a function inlines is in its
# size, and a function that is only ever inlined has none of its own (one
# engine may compile it alone and the other not: "same fns" compares just
# the functions both compiled).
# "all" sums every compile, recompiles after deopts included.
#
# Maglev's sizes don't change with cloison, so they're cached in
# perf-history/maglev-sizes.tsv by Node version and the file's contents
# (`git hash-object`); MAGLEV=1 measures them again.
#
# Node: $NODE, else the Node 26 scripts/tiers.sh downloads.
set -euo pipefail
cd "$(dirname "$0")/.."
NODE=${NODE:-$(ls -d ~/.local/share/cloison-v8/node-*/bin/node 2>/dev/null | tail -1)}
[[ -x $NODE ]] || { echo "no Node 26: run scripts/tiers.sh once, or set NODE" >&2; exit 1; }
cargo build --release --quiet --bin cloison

# `--pieces`: cloison's final code (as below: each function's last compile
# per entry) per stencil family, hot, out-of-line and exits apart
# (`CLOISON_JIT_LOG=pieces`), summed over the files, and per task of
# docs/maglev-size-tasks.md (the families each task shrinks). Table in
# perf-history/pieces-<commit>.md; `--record` appends the task totals to
# bench/maglev-tasks.tsv (`commit<TAB>task<TAB>bytes`) and every run shows
# the change since the last record.
if [[ ${1:-} == --pieces ]]; then
  shift
  record=false
  if [[ ${1:-} == --record ]]; then
    record=true
    shift
  fi
  commit=$(git rev-parse --short HEAD)
  git diff --quiet HEAD -- src stencils build.rs .cargo || commit="$commit+"
  files=("$@")
  [[ ${#files[@]} -gt 0 ]] || files=(bench/micro/*.js)
  mkdir -p perf-history
  python3 - "$commit" "$record" "${files[@]}" <<'EOF'
import collections, os, re, subprocess, sys

commit, record, *files = sys.argv[1:]
# The families each task shrinks (docs/maglev-size-tasks.md), by kind of
# code; S5 and S8 (encodings) touch every family.
TASKS = [
    ("S1 push", "hot|ool", r"array_push|wrapper:cloison_rt_array_push"),
    ("S2 calls", "hot|ool", r"direct_call|call_done|check_callee|callee_jne|construct_direct|call_rt|wrapper:cloison_rt_enter"),
    ("S3 entry clearing", "hot|ool", r"words_jne|clear|copy_word|zero"),
    ("S4 truthiness", "hot|ool", r"jtofalse|jtotrue|wrapper:cloison_rt_to_boolean"),
    ("S6 stores", "hot|ool", r"set_own|set_tr"),
    ("S7 allocation", "hot|ool", r"create_object|create_array|wrapper:cloison_rt_create_(object|array)"),
    ("S9 cell guards", "hot|ool", r"guard_word"),
    ("S10 heap-object checks", "hot|ool", r"guard_cell|check_acc_shape"),
    ("S11 untyped ===", "hot|ool", r"teq_j[tf]|wrapper:cloison_rt_loose_eq"),
    ("S12 moves", "hot|ool", r"star|ldar|spill|lda_constant|mov"),
    ("S13 keyed loads", "hot|ool", r"get_keyed_i_sh"),
    ("C1 exits", "exit", r".*"),
]

def family(name):
    """A stencil's name without its register variant (`spill_g1`, `get_own_nc_g0_to_g1`)."""
    if name.startswith("wrapper:"):
        return name
    slow = name.endswith("__slow")
    name = re.sub(r"__slow$", "", name)
    name = re.sub(r"_[gumfs]\d+.*$", "", name)
    name = re.sub(r"_[gumfs]$", "", name)
    return name + ("__slow" if slow else "")

total = collections.Counter()   # (kind, family) -> bytes
sites = collections.Counter()
for n, f in enumerate(files, 1):
    print(f"\r{n}/{len(files)} {os.path.basename(f):40}", end="", file=sys.stderr, flush=True)
    r = subprocess.run(["target/release/cloison", f], capture_output=True, text=True,
                       env={**os.environ, "CLOISON_JIT_LOG": "pieces"}, timeout=600)
    last, pending = {}, None
    for line in r.stderr.splitlines():
        if line.startswith("pieces "):
            pending = re.findall(r"(\w+):(\S+)=(\d+)/(\d+)", line)
        elif line.startswith("jit ") and pending is not None:
            m = re.match(r"jit (\S*) @(\d+)", line)
            last[(m.group(1), m.group(2) == "0")] = pending
            pending = None
    for ps in last.values():
        for kind, name, b, k in ps:
            total[(kind, family(name))] += int(b)
            sites[(kind, family(name))] += int(k)
print(file=sys.stderr)

kinds = ["hot", "ool", "exit"]
sums = {k: sum(v for (kk, _), v in total.items() if kk == k) for k in kinds}
tasks = []
for task, ks, pat in TASKS:
    b = sum(v for (k, fam), v in total.items() if re.fullmatch(ks, k) and re.match(pat, fam))
    tasks.append((task, b))
rows = [("all.hot", sums["hot"]), ("all.ool", sums["ool"]), ("all.exits", sums["exit"])] + tasks

tsv = "bench/maglev-tasks.tsv"
last = None
if os.path.exists(tsv):
    recs = [l.rstrip("\n").split("\t") for l in open(tsv)][1:]
    prev = [r[0] for r in recs if r[0] != commit]
    if prev:
        last = {r[1]: int(r[2]) for r in recs if r[0] == prev[-1]}
        last_commit = prev[-1]
lines = [f"cloison {commit}: final code per task of docs/maglev-size-tasks.md, bytes over {len(files)} files "
         f"(`scripts/vs-maglev.sh --pieces`)" + (f", change since {last_commit}" if last else ""), "",
         "| task | bytes | before | change |", "|---|---:|---:|---:|"]
for t, b in rows:
    before = last.get(t) if last else None
    ch = f"{100 * (b - before) / before:+.1f}%" if before else ""
    lines.append(f"| {t} | {b:,} | {'' if before is None else f'{before:,}'} | {ch} |")
lines += ["", "**Per stencil family** (bytes / pieces; largest first)", "",
          "| family | hot | ool | exits |", "|---|---:|---:|---:|"]
fams = sorted({fam for _, fam in total}, key=lambda fam: -sum(total[(k, fam)] for k in kinds))
cell = lambda k, fam: f"{total[(k, fam)]:,} / {sites[(k, fam)]}" if total[(k, fam)] else ""
lines += [f"| {fam} | {cell('hot', fam)} | {cell('ool', fam)} | {cell('exit', fam)} |" for fam in fams]
out = f"perf-history/pieces-{commit}.md"
open(out, "w").write("\n".join(lines) + "\n")
print("\n".join(lines[:len(rows) + 4]))
print(f"families in {out}")
if record == "true":
    with open(tsv, "a") as o:
        if not os.path.exists(tsv) or os.path.getsize(tsv) == 0:
            o.write("commit\ttask\tbytes\n")
        for t, b in rows:
            o.write(f"{commit}\t{t}\t{b}\n")
    print(f"recorded {commit} in {tsv}")
EOF
  exit
fi

if [[ ${1:-} == --asm ]]; then
  file=${2:?file}; fn=${3:?function}; v8fn=${4:-${fn%%:*}}
  dump=$(mktemp); trap 'rm -f "$dump"' EXIT
  CLOISON_JIT_DUMP=$dump CLOISON_JIT_DUMP_FN=$fn target/release/cloison "$file" >/dev/null
  if [[ ! -s $dump ]]; then
    echo "=== cloison: $fn not compiled on its own (inlined into its callers, or never hot)"
  else
    echo "=== cloison: $fn, last compile, hot code ($(stat -c %s "$dump") bytes)"
  fi
  [[ -s $dump ]] && objdump -D -b binary -m i386:x86-64 -M intel --no-show-raw-insn "$dump" | sed -n '/<.data>:/,$p' | tail -n +2
  echo
  # The last Maglev code object named `v8fn` or `X.v8fn` (its printed code
  # has no source position: name `X.m` to choose between several `m`).
  "$NODE" --max-opt=2 --single-threaded --print-maglev-code "$file" 2>&1 | awk -v fn="$v8fn" '
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

def cloison(f):
    r = subprocess.run(["target/release/cloison", f], capture_output=True, text=True,
                       env={**os.environ, "CLOISON_JIT_LOG": "1"}, timeout=600)
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
    compiles = []
    for m in os.listdir(out):
        path = os.path.join(out, m)
        for line in open(path):
            # `addr size JS:<tier>name file:line:col`: `+` Maglev, `o+` its
            # on-stack replacement, `~` Ignition, `^` Sparkplug.
            p = line.split(" ", 2)
            if len(p) == 3 and os.path.abspath(f) in p[2]:
                mm = re.match(r"JS:(o?)([~^+*])'?(.*) (\S+)$", p[2].strip())
                if mm:
                    # (A script's top level has no name: cloison's `<script>`.)
                    name = mm.group(3).rsplit(".", 1)[-1] or "<script>"
                    name += ":" + ":".join(mm.group(4).rsplit(":", 2)[1:])
                    if mm.group(2) == "+":
                        compiles.append((name, "osr" if mm.group(1) else "entry", int(p[1], 16)))
        os.remove(path)
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
    # (Rows cached before functions had positions are measured again.)
    if remeasure or key not in cache or not any(":" in c[0] for c in cache[key] if c[1] != "none"):
        cache[key] = maglev(f) or [("-", "none", 0)]
        added = True
    mfin, mall = final([c for c in cache[key] if c[1] in ("entry", "osr")])
    ok, (hfin, _), (cfin, call), (efin, _) = cloison(f)
    names = sorted(set(mfin) | set(cfin), key=lambda k: -max(cfin[k], mfin[k]))
    both = [k for k in names if k in mfin and k in cfin]
    s = lambda c, ks: sum(c[k] for k in ks)
    rows.append(dict(bench=b, ok=ok,
        cloison_hot=sum(hfin.values()), cloison_code=sum(cfin.values()), cloison_exits=sum(efin.values()),
        maglev=sum(mfin.values()), both_code=s(cfin, both), both_maglev=s(mfin, both),
        cloison_all=sum(call.values()), maglev_all=sum(mall.values())))
    details[b] = [(k, hfin.get(k), cfin.get(k), efin.get(k), mfin.get(k)) for k in names]
print(file=sys.stderr)
if added:
    with open(cache_path, "w") as out:
        out.write("v8\tblob\tfunction\tkind\tbytes\n")
        for (ver, h), cs in sorted(cache.items()):
            for name, kind, size in cs:
                out.write(f"{ver}\t{h}\t{name}\t{kind}\t{size}\n")

ratio = lambda a, b: f"{a / b:.2f}" if a and b else "-"
num = lambda v: "-" if not v else f"{v:,}"
lines = [f"cloison {commit} against Maglev (V8 {v8}, `--max-opt=2`): final machine code in bytes, every "
         "function's last compile of each entry summed (`scripts/vs-maglev.sh`). cloison: hot plus out-of-line code "
         "(edge stubs, slow-path helpers, runtime-call wrappers), what Maglev's size counts (its deferred code); "
         "cloison hot: just the hot part; exits apart (Maglev's: a 4-byte call each, the frame in metadata). "
         "Ratios cloison/Maglev (above 1: cloison's is larger). Code a function inlines counts in it, and Maglev often "
         "also compiles on their own functions cloison only inlines (see the per-function tables). \"same fns\": "
         "cloison/M over just the functions both compiled on their own (matched by `name:line:col`). \"all\": "
         "every compile's cloison code, recompiles included.", "",
         "| benchmark | cloison | cloison hot | exits | Maglev | cloison/M | hot/M | +exits/M | same fns cloison/M | cloison all | Maglev all |",
         "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
keys = ["bench", "ok", "cloison_code", "cloison_hot", "cloison_exits", "maglev", "both_code", "both_maglev",
        "cloison_all", "maglev_all"]
tsv = ["\t".join(keys)]
for r in rows:
    flag = "" if r["ok"] else " (wrong result)"
    lines.append(f"| {r['bench']}{flag} | {num(r['cloison_code'])} | {num(r['cloison_hot'])} | {num(r['cloison_exits'])} | "
                 f"{num(r['maglev'])} | {ratio(r['cloison_code'], r['maglev'])} | {ratio(r['cloison_hot'], r['maglev'])} | "
                 f"{ratio(r['cloison_code'] + r['cloison_exits'], r['maglev'])} | {ratio(r['both_code'], r['both_maglev'])} | "
                 f"{num(r['cloison_all'])} | {num(r['maglev_all'])} |")
    tsv.append("\t".join(str(r[k]) for k in keys))
for r in rows:
    r["cloison_code_exits"] = r["cloison_code"] + r["cloison_exits"]
tot = {k: sum(r[k] for r in rows if r["ok"]) for k in ["cloison_code", "cloison_hot", "cloison_exits", "maglev"]}
matched = [r for r in rows if r["ok"] and r["maglev"] and r["cloison_code"]]
gm = lambda key: math.exp(sum(math.log(r[key] / r["maglev"]) for r in matched) / len(matched)) if matched else 0
lines += ["", f"Sum (right results): cloison {tot['cloison_code']:,} (hot {tot['cloison_hot']:,}), exits {tot['cloison_exits']:,}, "
          f"Maglev {tot['maglev']:,}; geomean cloison/M {gm('cloison_code'):.2f}, hot/M {gm('cloison_hot'):.2f}, "
          f"+exits/M {gm('cloison_code_exits'):.2f} over the {len(matched)} files both compiled.", ""]
lines += ["**Per function** (final code, bytes; largest first)", ""]
for b, ds in details.items():
    lines += [f"<details><summary>{b}</summary>", "", "| function | cloison | cloison hot | exits | Maglev | cloison/M |",
              "|---|---:|---:|---:|---:|---:|"]
    lines += [f"| {k} | {num(c)} | {num(h)} | {num(e)} | {num(m)} | {ratio(c, m)} |" for k, h, c, e, m in ds]
    lines += ["", "</details>", ""]
out = f"perf-history/vs-maglev-{commit}"
open(out + ".md", "w").write("\n".join(lines) + "\n")
open(out + ".tsv", "w").write("\n".join(tsv) + "\n")
# The terminal gets the summary table, by cloison/Maglev, largest first.
print(f"{'benchmark':40} {'cloison':>8} {'cloison hot':>8} {'exits':>8} {'Maglev':>8} {'cloison/M':>6} {'hot/M':>6} {'same fns':>8}")
for r in sorted(rows, key=lambda r: -(r["cloison_code"] / r["maglev"]) if r["maglev"] else 0):
    print(f"{r['bench'] + ('' if r['ok'] else ' !'):40} {r['cloison_code']:8,} {r['cloison_hot']:8,} {r['cloison_exits']:8,} "
          f"{r['maglev']:8,} {ratio(r['cloison_code'], r['maglev']):>6} {ratio(r['cloison_hot'], r['maglev']):>6} "
          f"{ratio(r['both_code'], r['both_maglev']):>8}")
print(next(l for l in lines if l.startswith("Sum")))
print(f"tables in {out}.md and .tsv")
EOF
