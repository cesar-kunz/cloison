#!/usr/bin/env python3
"""Check the stencils against their targets.

    scripts/stencil-targets.py [--arch x86_64|aarch64] [--check]
                               [--counts FILE ...] [--all] [--unannotated N]
    scripts/stencil-targets.py [--arch …] --show KEY-GLOB ...
    scripts/stencil-targets.py [--arch …] --disasm NAME ...

A target is the instruction count of the code a person would write for a
stencil's hot path (see docs/opcode-targets.md, docs/arm64-targets.md). It is
written next to the stencil in stencils/stencils.rs as a comment:

    // target <arch> <key>: <n> [now <m>] <ideal code or note>

- <arch> is x86_64 or aarch64.
- <key> is a glob over stencil keys: the stencil's name with each slot token
  written as build.rs's templates write it (`_g{}`, `_u{}`, `_f{}`), so
  `get_own_g{}` is every `get_own_g<k>`, and `tget_*_chk_g{}` every element
  width's. Several globs are separated by commas; one starting with `!`
  excludes (`tget_*_g{},!*_chk_*`).
- <n> is the target. `now <m>` records a known gap: the stencil is <m>
  instructions and the target <n> is not reached yet.
- `fallthrough <k>` says the target is for another path than the one the
  count follows: LLVM laid out a different path (a number store, the
  runtime call) as the fall-through, and that one is <k> instructions. The
  note names the path the target is for. The stencil is reported `path`
  while the count is <k>, and as changed (a failure) when it is not, to be
  judged again.

A count is the stencil's hot path, as docs/opcode-targets.md counts it:
the instructions from its start, as the JIT copies it (after build.rs's
rewrites), to its first unconditional jump, that jump left out when it goes
to `cloison_continue` (the JIT drops it when the next piece follows). What
follows that jump is cold: exits moved off the fall-through, slow paths.
`--show` gives the whole stencil's count too, as scripts/stencil-listing.py
counts it. x86-64 is decoded with GNU objdump; on arm64 every instruction
is 4 bytes and an unconditional `b` is recognized by its encoding.

The report: each annotation's stencils against it (ok, gap, over target
beyond a known gap, under the target, `path` or `changed` for a
`fallthrough`, or matching nothing), then the
families with no target for that architecture. With `--counts` (the
output of `CLOISON_COUNT=1 cloison FILE.js`, any number of files), the
families are ranked by their share of the code executed, averaged over the
files; otherwise by name. `--check` exits 1 when a stencil is over its
target beyond a known gap, a `fallthrough` count changed, or an
annotation matches nothing.
"""
import collections, fnmatch, glob, os, re, subprocess, sys, tempfile

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
SOURCE = os.path.join(ROOT, "stencils", "stencils.rs")
TRIPLES = {"aarch64": ["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin"]}

def table_path(arch):
    host = "aarch64" if os.uname().machine in ("arm64", "aarch64") else "x86_64"
    dirs = [os.path.join(ROOT, "target", "release")] if arch == host else []
    dirs += [os.path.join(ROOT, "target", t, "release") for t in TRIPLES.get(arch, [])]
    paths = []
    for d in dirs:
        paths += glob.glob(os.path.join(d, "build", "cloison", "*", "out", "stencils.rs"))
        paths += glob.glob(os.path.join(d, "build", "cloison-*", "out", "stencils.rs"))
    return max(paths, key=os.path.getmtime) if paths else None

def entries(path):
    src = open(path).read()
    out = []
    for m in re.finditer(r'Stencil \{ name: "([^"]*)", slow: \w+, slow_at: \d+, code: &\[([^\]]*)\], relocs: &\[(.*?)\], template:', src):
        code = bytes(int(x) for x in m.group(2).split(",") if x.strip())
        cont = [int(r.group(1)) for r in re.finditer(r'R\((\d+), \d+, Sym::Continue, -?\d+\)', m.group(3))]
        out.append((m.group(1), code, cont))
    return out

def key(name):
    """`addr_ii_u3_u5` -> `addr_ii_u{}_u{}`, as build.rs's `slot_params`."""
    toks = name.split("_")
    # (`tget_u8`, `tget_f64`: an element type, not a slot.)
    typed = toks[0] in ("tget", "tset")
    return "_".join(t if i == 0 or (typed and i == 1) or not re.fullmatch(r"[guf]\d+", t) else t[0] + "{}"
                    for i, t in enumerate(toks))

def counts_x86(ents):
    """(hot, total) instruction counts, decoding every stencil in one objdump run."""
    blob, starts = bytearray(), []
    for _, code, _ in ents:
        starts.append(len(blob))
        blob += code
    with tempfile.NamedTemporaryFile(suffix=".bin") as f:
        f.write(blob)
        f.flush()
        out = subprocess.run(["objdump", "-D", "-b", "binary", "-m", "i386:x86-64", "--no-show-raw-insn", f.name],
                             capture_output=True, text=True, check=True).stdout
    insns = [(int(m.group(1), 16), m.group(2)) for m in re.finditer(r"^\s*([0-9a-f]+):\t(\S+)", out, re.M)]
    res, j = [], 0
    for (name, code, cont), s in zip(ents, starts):
        e = s + len(code)
        while j < len(insns) and insns[j][0] < s:
            j += 1
        ins = []
        while j < len(insns) and insns[j][0] < e:
            ins.append((insns[j][0] - s, insns[j][1]))
            j += 1
        # (`jmp rel32` to the continuation: its relocation is one byte in.)
        total = len(ins) - (1 if ins and ins[-1][0] + 1 in cont else 0)
        hot = len(ins)
        for k, (off, mn) in enumerate(ins):
            if mn == "jmp":
                hot = k + (0 if off + 1 in cont else 1)
                break
        res.append((hot, total))
    return res

def disasm(arch, code):
    m = "i386:x86-64" if arch == "x86_64" else "aarch64"
    with tempfile.NamedTemporaryFile(suffix=".bin") as f:
        f.write(code)
        f.flush()
        r = subprocess.run(["objdump", "-D", "-b", "binary", "-m", m, "-M", "intel", f.name], capture_output=True, text=True)
    lines = [l for l in r.stdout.splitlines() if re.match(r"\s*[0-9a-f]+:\t", l)]
    if lines:
        return "\n".join(lines)
    return "\n".join(f"  {k:4x}: {int.from_bytes(code[k:k + 4], 'little'):08x}" for k in range(0, len(code) - 3, 4))

def counts_arm64(ents):
    res = []
    for _, code, cont in ents:
        words = [int.from_bytes(code[k:k + 4], "little") for k in range(0, len(code) - 3, 4)]
        total = len(words) - (1 if words and len(code) - 4 in cont else 0)
        hot = len(words)
        for k, w in enumerate(words):
            # `b imm26`, `br xN`, `ret`.
            if w & 0xFC000000 == 0x14000000 or w & 0xFFFFFC1F in (0xD61F0000, 0xD65F0000):
                hot = k + (0 if 4 * k in cont else 1)
                break
        res.append((hot, total))
    return res

def annotations():
    anns = []
    for i, line in enumerate(open(SOURCE), 1):
        m = re.match(r"\s*//[/!]?\s*target (x86_64|aarch64) (\S+): (\d+)(?: now (\d+))?(?: fallthrough (\d+))?\s*(.*)$", line)
        if m:
            anns.append(dict(arch=m.group(1), glob=m.group(2), target=int(m.group(3)),
                             now=int(m.group(4)) if m.group(4) else None, fall=int(m.group(5)) if m.group(5) else None, note=m.group(6), line=i))
    return anns

def matches(k, pattern):
    globs = pattern.split(",")
    return (any(fnmatch.fnmatchcase(k, g) for g in globs if not g.startswith("!"))
            and not any(fnmatch.fnmatchcase(k, g[1:]) for g in globs if g.startswith("!")))

def family(k):
    k = k.replace("__slow", "")
    k = re.sub(r"_to_g\{\}$", "", k)
    k = re.sub(r"_s\d+$", "", k)
    prev = None
    while prev != k:
        prev = k
        k = re.sub(r"_(?:m|g\{\}|u\{\}|f\{\}|k16)$", "", k)
    return re.sub(r"_\d+$", "", k)

def weights(files):
    w = collections.Counter()
    for f in files:
        for line in open(f):
            m = re.match(r"\s*([\d.]+)%\s+\d+ x\s+\d+ B\s+(\S+)", line)
            if m:
                w[family(key(m.group(2)))] += float(m.group(1)) / len(files)
    return w

def main(argv):
    archs, check, files, show_all, top, show, dis = ["x86_64", "aarch64"], False, [], False, 40, [], []
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--arch":
            archs = [argv[i + 1]]; i += 1
        elif a == "--check":
            check = True
        elif a == "--all":
            show_all = True
        elif a == "--unannotated":
            top = int(argv[i + 1]); i += 1
        elif a == "--disasm":
            while i + 1 < len(argv) and not argv[i + 1].startswith("--"):
                dis.append(argv[i + 1]); i += 1
        elif a == "--show":
            while i + 1 < len(argv) and not argv[i + 1].startswith("--"):
                show.append(argv[i + 1]); i += 1
        elif a == "--counts":
            while i + 1 < len(argv) and not argv[i + 1].startswith("--"):
                files.append(argv[i + 1]); i += 1
        else:
            sys.exit(__doc__)
        i += 1
    anns, failed = annotations(), False
    w = weights(files) if files else None
    for arch in archs:
        path = table_path(arch)
        if not path:
            print(f"== {arch}: no generated table (build for it first)\n")
            continue
        ents = entries(path)
        ns = counts_x86(ents) if arch == "x86_64" else counts_arm64(ents)
        by_key = collections.defaultdict(list)
        for (name, _, _), (hot, total) in zip(ents, ns):
            by_key[key(name)].append((name, hot, total))
        if dis:
            for name, code, _ in ents:
                if name in dis or key(name) in dis:
                    print(f"== {arch} {name}: {len(code)} bytes")
                    print(disasm(arch, code))
            continue
        if show:
            print(f"== {arch}")
            for k in sorted(by_key):
                if any(matches(k, g) for g in show):
                    hs = sorted({h for _, h, _ in by_key[k]})
                    ts = sorted({t for _, _, t in by_key[k]})
                    print(f"  {k:32} {'/'.join(map(str, hs)):>6}   (whole: {'/'.join(map(str, ts))})")
            continue
        mine = [a for a in anns if a["arch"] == arch]
        print(f"== {arch}: {len(ents)} stencils ({len(by_key)} keys), {len(mine)} targets, from {os.path.relpath(path, ROOT)}")
        if os.path.getmtime(path) < os.path.getmtime(SOURCE):
            print("  (the table is older than stencils/stencils.rs: build for this target again)")
        covered = set()
        tally = collections.Counter()
        for a in mine:
            hit = [(k, v) for k, v in by_key.items() if matches(k, a["glob"])]
            covered |= {family(k) for k, _ in hit}
            if not hit:
                status, detail = "stale", "matches no stencil"
                failed = True
            else:
                cs = [n for _, v in hit for _, n, _ in v]
                lo, hi = min(cs), max(cs)
                worst = [nm for _, v in hit for nm, n, _ in v if n == hi][:3]
                rng = f"{lo}" if lo == hi else f"{lo}..{hi}"
                if a["fall"] is not None:
                    status = "path" if lo == hi == a["fall"] else "changed"
                    failed |= status == "changed"
                elif hi <= a["target"]:
                    status = "under" if hi < a["target"] else "ok"
                elif a["now"] is not None and hi <= a["now"]:
                    status = "gap"
                else:
                    status = "over"
                    failed = True
                detail = f"is {rng}" + (f" (worst: {', '.join(worst)})" if status in ("over", "gap") else "")
            tally[status] += 1
            if show_all or status not in ("ok",):
                now = f", known {a['now']}" if a["now"] is not None else ""
                now += f", fall-through {a['fall']}" if a["fall"] is not None else ""
                print(f"  {status:5} {a['glob']:28} target {a['target']}{now}; {detail}  [stencils.rs:{a['line']}]")
        print("  " + ", ".join(f"{k} {v}" for k, v in sorted(tally.items())))
        fams = collections.defaultdict(int)
        for k, v in by_key.items():
            fams[family(k)] += len(v)
        un = [f for f in fams if f not in covered]
        print(f"  {len(un)} of {len(fams)} families have no {arch} target")
        if w is not None:
            ranked = sorted(un, key=lambda f: -w.get(f, 0))
            print(f"  by share of the code executed (mean of {len(files)} runs):")
            for f in ranked[:top]:
                if w.get(f, 0) > 0:
                    print(f"    {w[f]:5.2f}%  {f} ({fams[f]} stencils)")
        else:
            print("    " + " ".join(sorted(un)[:top]) + (" …" if len(un) > top else ""))
        print()
    if check and failed:
        sys.exit(1)

main(sys.argv[1:])
