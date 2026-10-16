#!/usr/bin/env bash
# Snapshot the stencils' machine code per toolchain, to catch an LLVM
# update that makes them worse. rust-toolchain.toml follows nightly, so
# LLVM changes with `rustup update`; a snapshot is keyed by the LLVM
# version and the nightly's commit date.
#
#   scripts/stencil-snapshot.sh [aarch64]         # snapshot this toolchain's stencils
#   scripts/stencil-snapshot.sh --compare [A B]   # stencils whose code changed between
#                                                 # two snapshots (default: the newest two)
#   scripts/stencil-snapshot.sh --diff NAME [A B] # one stencil's code in both
#
# Snapshots go to notes/stencils/<key>/<arch>/ (untracked): `code.txt`, each
# stencil's disassembly with addresses stripped, and `summary.tsv`
# (stencil, instructions, bytes). --compare takes the arch from ARCH
# (default x86_64).
set -euo pipefail
cd "$(dirname "$0")/.."
base=notes/stencils
arch=${ARCH:-x86_64}

snapshots() { ls -1d "$base"/*/"$arch" 2>/dev/null | sort; }
pick() { # pick A B: the two snapshot dirs (given, or the newest two)
  if [[ $# -ge 2 ]]; then echo "$base/$1/$arch" "$base/$2/$arch"; else snapshots | tail -2 | tr '\n' ' '; fi
}

case ${1:-} in
--compare)
  shift
  read -r a b <<<"$(pick "$@")"
  [[ -n ${b:-} ]] || { echo "need two snapshots of $arch" >&2; exit 1; }
  python3 - "$a" "$b" <<'EOF'
import sys
def load(d):
    rows = {}
    for line in list(open(f"{d}/summary.tsv"))[1:]:
        n, i, b = line.split("\t")
        rows[n] = (int(i), int(b))
    return rows
a, b = sys.argv[1], sys.argv[2]
A, B = load(a), load(b)
print(f"{a} -> {b}")
changed = sorted(((B[n][0] - A[n][0], B[n][1] - A[n][1], n) for n in A.keys() & B.keys()
                  if A[n] != B[n]), key=lambda t: (-t[0], -t[1], t[2]))
worse = [c for c in changed if c[0] > 0 or (c[0] == 0 and c[1] > 0)]
better = [c for c in changed if c not in worse]
for title, rows in (("longer", worse), ("shorter", better)):
    print(f"{len(rows)} {title}:")
    for di, db, n in rows[:40]:
        print(f"  {n:48} {A[n][0]:4} -> {B[n][0]:4} insns ({di:+d}), {db:+d} bytes")
gone, new = sorted(A.keys() - B.keys()), sorted(B.keys() - A.keys())
if gone or new:
    print(f"only in the first: {len(gone)}, only in the second: {len(new)}")
ta, tb = sum(v[0] for v in A.values()), sum(v[0] for v in B.values())
print(f"total instructions: {ta} -> {tb} ({tb - ta:+d})")
EOF
  exit 0
  ;;
--diff)
  name=$2
  shift 2
  read -r a b <<<"$(pick "$@")"
  extract() { awk -v n="<cloison_stencil_$name>:" '$0 == n {p = 1; next} /^<cloison_stencil_/ {p = 0} p' "$1/code.txt"; }
  diff -u --label "$a" --label "$b" <(extract "$a") <(extract "$b") || true
  exit 0
  ;;
"" | x86_64 | aarch64) arch=${1:-x86_64} ;;
*) sed -n '2,16p' "$0"; exit 2 ;;
esac

case $arch in
  x86_64)  triple=x86_64-unknown-linux-gnu;   objdump=(objdump -M intel) ;;
  aarch64) triple=aarch64-unknown-linux-musl; objdump=(aarch64-linux-gnu-objdump) ;;
esac
llvm=$(rustc -vV | sed -n 's/^LLVM version: //p')
date=$(rustc -vV | sed -n 's/^commit-date: //p')
out=$base/llvm-$llvm-nightly-$date/$arch
mkdir -p "$out"
cargo build --release --quiet --lib --target "$triple"
obj=$(ls -t target/"$triple"/release/build/cloison/*/out/stencils.o | head -1)
# One block per address (helpers included): `<name>:` (and `<alias>:` for
# stencils LLVM merged into it, being identical), then its instructions,
# without addresses, relocations folded onto their instruction. The
# summary lists every stencil symbol, aliases with their target's counts.
"${objdump[@]}" -dr --no-show-raw-insn "$obj" >"$out/.dis"
# value, section, size, name of each stencil symbol.
objdump -t "$obj" | awk '$NF ~ /^cloison_stencil_/ && $3 == "F" { print $1, $4, $5, $NF }' >"$out/.syms"
python3 - "$out" <<'PY'
import collections, re, sys
out = sys.argv[1]
# Sections start at 0 in a relocatable object: key blocks by (section, address).
blocks, order, cur = {}, [], None
section = None
for line in open(f"{out}/.dis"):
    line = line.rstrip("\n")
    m = re.match(r"^Disassembly of section (\S+):", line)
    if m:
        section = m.group(1)
        continue
    m = re.match(r"^([0-9a-f]+) <(cloison_stencil_[^>]*)>:$", line)
    if m:
        cur = (section, m.group(2))
        blocks[cur] = []
        order.append(cur)
        continue
    if not line.strip():
        cur = None
        continue
    m = re.match(r"^\s+[0-9a-f]+:\s+(.*)$", line)
    if cur and m:
        ins = m.group(1)
        if ins.startswith("R_"):
            blocks[cur].append("    ; " + re.sub(r"\s+", " ", ins.strip()))
        else:
            blocks[cur].append("  " + re.sub(r"\s+", " ", ins))
# Each symbol is in a section of its own (`.text.cloison_stencil_<name>`);
# a merged one is an alias at the same address of another's section.
syms = [l.split() for l in open(f"{out}/.syms")]
names = [n for _, _, _, n in syms]
size = {n: int(z, 16) for _, _, z, n in syms}
where = {n: (sec, v) for v, sec, _, n in syms}
by_name = {name: code for (_, name), code in blocks.items()}
labeled = set(by_name)
at = {where[m]: m for m in labeled if m in where}
aliases = collections.defaultdict(list)
for n in names:
    if n not in labeled and where[n] in at:
        aliases[at[where[n]]].append(n)
with open(f"{out}/code.txt", "w") as f:
    for (_, name) in order:
        for a in [name] + sorted(aliases.get(name, [])):
            f.write(f"<{a}>:\n")
        f.write("\n".join(by_name[name]) + "\n")
with open(f"{out}/summary.tsv", "w") as f:
    f.write("stencil\tinstructions\tbytes\n")
    rows = []
    for name, code in by_name.items():
        n = sum(1 for l in code if not l.startswith("    ;"))
        for a in [name] + aliases.get(name, []):
            rows.append((a, n, size.get(a, 0)))
    for a, n, b in sorted(rows):
        f.write(f"{a}\t{n}\t{b}\n")
PY
rm -f "$out/.dis" "$out/.syms"
echo "$(($(wc -l <"$out/summary.tsv") - 1)) stencils, $(awk -F'\t' 'NR > 1 { s += $2 } END { print s }' "$out/summary.tsv") instructions -> $out"
