#!/bin/bash
# The GC hazard analysis (tools/gc-hazards): a cell held in a Rust local
# across a call that may collect, rooted nowhere else. Rust frames' native
# words no longer mark (docs/gc.md), so each is a use after free waiting
# for its collection. Checks the hazards found against the reviewed ones
# (tools/gc-hazards/baseline.txt): a new one fails; one gone is to be
# dropped from it.
#   scripts/gc-hazards.sh            check
#   scripts/gc-hazards.sh --update   rewrite the baseline
# Needs the pinned toolchain's `rustc-dev` (rustup component add rustc-dev
# llvm-tools-preview). The analysis' assumptions: tools/gc-hazards/annotations.txt.
set -euo pipefail
cd "$(dirname "$0")/.."
target=${GC_HAZARDS_TARGET:-target/gc-hazards}
baseline=tools/gc-hazards/baseline.txt
# Hazards reviewed and safe: a baseline line, a tab, `# ` and why.
safe=tools/gc-hazards/safe.txt
mkdir -p "$target"
out=$(realpath "$target")/hazards.txt

# The driver links rustc's libraries dynamically: not with the static host
# flags of .cargo/config.toml (RUSTFLAGS overrides them).
RUSTFLAGS="-Cdebuginfo=0" cargo build --quiet --release --manifest-path tools/gc-hazards/Cargo.toml \
    --target-dir "$target/tool"
sysroot=$(rustc --print sysroot)
# (A check cargo finds fresh runs no rustc: forget it.)
find "$target/debug" -path '*fingerprint*' -name '*lib-cloison*' -delete 2>/dev/null || true
rm -f "$out"
LD_LIBRARY_PATH="$sysroot/lib" RUSTC_WORKSPACE_WRAPPER="$(realpath "$target")/tool/release/gc-hazards" \
    GC_HAZARDS_OUT="$out" GC_HAZARDS_ANNOTATIONS="$PWD/tools/gc-hazards/annotations.txt" \
    cargo check --quiet --lib --target-dir "$target"
[ -f "$out" ] || { echo "gc-hazards: the analysis wrote nothing" >&2; exit 1; }

# A hazard's key: its function, value and callee (not its line; a
# temporary's number neither).
keys() { awk -F'\t' 'NF { print $2 "\t" $3 }' "$1" | sed -E 's/`_[0-9]+:/`_:/; s/\{closure@[^}]*\}/{closure}/g' | sort -u; }
if [ "${1:-}" = --update ]; then
    {
        echo "# The GC hazards scripts/gc-hazards.sh found and that are not fixed yet: each"
        echo "# to be fixed (rooted: Vm::rooted, Vm::root_scope) or, if safe, explained"
        echo "# in annotations.txt. Function, then the hazard. (--update rewrites it.)"
        comm -23 <(keys "$out") <(grep -v '^#' "$safe" | sed -E 's/\t# .*$//' | sort -u)
    } > "$baseline"
    echo "gc-hazards: $(grep -vc '^#' "$baseline") in $baseline"
    exit 0
fi
# The baseline's and the safe list's keys (the safe list's without its reason).
known() { { grep -v '^#' "$baseline"; grep -v '^#' "$safe" | sed -E 's/\t# .*$//'; } | grep . | sort -u; }
new=$(comm -13 <(known) <(keys "$out"))
gone=$(comm -23 <(known) <(keys "$out"))
if [ -n "$gone" ]; then
    echo "gc-hazards: no longer found (drop them from $baseline or $safe: --update does the baseline):"
    echo "$gone" | sed 's/^/  /'
fi
if [ -n "$new" ]; then
    echo "gc-hazards: new hazards (root the value, or explain why it is safe in annotations.txt):"
    while IFS= read -r k; do
        f=$(echo "$k" | cut -f1)
        awk -F'\t' -v f="$f" '$2 == f' "$out" | sed -E 's/`_[0-9]+:/`_:/' | grep -F -- "$(echo "$k" | cut -f2)" | sed 's/^/  /'
    done <<< "$new"
    exit 1
fi
echo "gc-hazards: $(keys "$out" | wc -l) known, none new"
