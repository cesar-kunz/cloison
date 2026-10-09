#!/usr/bin/env bash
# Run each Octane benchmark under toi, deterministically (as Octane's own
# `doDeterministic` runs them), and print whether it runs to the end, or
# the first error it stops at.
#
#   scripts/octane.sh                # every benchmark
#   scripts/octane.sh crypto splay   # only these
#   SCORE=1 scripts/octane.sh        # also the score of the ones that run,
#                                    # next to V8's (Node, as scripts/tiers.sh)
#
# Octane is cloned into $OCTANE (default notes/octane, not tracked) at a
# fixed commit. A benchmark's script is base.js, its files, and a driver;
# they go to target/octane/. A run's value is 1000 plus one per benchmark
# in the suite (Splay has two: SplayLatency); anything else is an error.
#
# The language features and builtins they're missing have tests of their
# own: tests/octane/ (cargo test --test octane).
set -euo pipefail
cd "$(dirname "$0")/.."
octane=${OCTANE:-notes/octane}
commit=570ad1ccfe86e3eecba0636c8f932ac08edec517
timeout=${TIMEOUT:-600}
if [ ! -d "$octane" ]; then
  git clone -q https://github.com/chromium/octane.git "$octane"
  git -C "$octane" checkout -q "$commit"
fi
out=target/octane
mkdir -p "$out"
cargo build -q --release

declare -A files=(
  [richards]="richards" [deltablue]="deltablue" [crypto]="crypto"
  [raytrace]="raytrace" [earley-boyer]="earley-boyer" [regexp]="regexp"
  [splay]="splay" [navier-stokes]="navier-stokes" [pdfjs]="pdfjs"
  [mandreel]="mandreel" [gbemu]="gbemu-part1 gbemu-part2"
  [code-load]="code-load" [box2d]="box2d" [zlib]="zlib zlib-data"
  [typescript]="typescript typescript-input typescript-compiler"
)
order="richards deltablue crypto raytrace earley-boyer regexp splay
  navier-stokes pdfjs mandreel gbemu code-load box2d zlib typescript"
[ $# -gt 0 ] && order="$*"

# The value is the script's completion value (toi prints it; Node's
# wrapper below too).
deterministic='var done = 0, failed = "";
BenchmarkSuite.config.doDeterministic = true;
BenchmarkSuite.RunSuites({
  NotifyResult: function (name, result) { done++; },
  NotifyError: function (name, error) { if (failed == "") failed = name + ": " + error; },
  NotifyScore: function (score) { done += 1000; }
});
failed == "" ? done : failed;'
score='var score = "", failed = "";
BenchmarkSuite.RunSuites({
  NotifyResult: function (name, result) {},
  NotifyError: function (name, error) { if (failed == "") failed = name + ": " + error; },
  NotifyScore: function (s) { score = s; }
});
failed == "" ? score : failed;'

node_value() {
  "$NODE" -e 'const r = require("vm").runInThisContext(require("fs").readFileSync(process.argv[1], "utf8")); console.log(String(r));' "$1"
}
if [ -n "${SCORE:-}" ]; then
  NODE=${NODE:-$(ls -d ~/.local/share/toi-v8/node-*/bin/node 2>/dev/null | tail -1)}
  NODE=${NODE:-node}
fi

printf '%-14s %s\n' benchmark result
for b in $order; do
  for kind in run score; do
    f=$out/$b-$kind.js
    { cat "$octane/base.js"; for p in ${files[$b]}; do cat "$octane/$p.js"; echo; done
      if [ $kind = run ]; then echo "$deterministic"; else echo "$score"; fi; } > "$f"
  done
  r=$(timeout "$timeout" ./target/release/toi "$out/$b-run.js" 2>&1 | tail -n 1 | cut -c1-160) || true
  [ -z "$r" ] && r="(timed out or crashed)"
  if [[ "$r" =~ ^100[1-9]$ ]]; then
    line="runs"
    if [ -n "${SCORE:-}" ]; then
      s=$(timeout "$timeout" ./target/release/toi "$out/$b-score.js" 2>&1 | tail -n 1)
      v=$(node_value "$out/$b-score.js" 2>&1 | tail -n 1)
      line="runs, score $s (V8 $v)"
    fi
  else
    line="$r"
  fi
  printf '%-14s %s\n' "$b" "$line"
done
