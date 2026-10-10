#!/usr/bin/env bash
# The collector's microbenchmarks (bench/gc/*.js), one line each: wall
# seconds, peak RSS, the largest live data a full collection found, the share of the time spent in pauses, the minor and major collections'
# counts and longest pauses, and whether the result matched `expect`.
#
#   scripts/gc-micro.sh                    # every file
#   scripts/gc-micro.sh bench/gc/cards.js  # only these
#   TOI=path/to/toi scripts/gc-micro.sh    # another build
set -uo pipefail
cd "$(dirname "$0")/.."
toi=${TOI:-target/release/toi}
files=("$@")
[ $# -gt 0 ] || files=(bench/gc/*.js)
printf '%-16s %7s %6s %6s %6s %13s %13s  %s\n' file seconds MB live gc% "minors (max)" "majors (max)" result
for f in "${files[@]}"; do
  out=$(mktemp)
  if [ "$(uname)" = Darwin ]; then
    TOI_GC_STATS=1 /usr/bin/time -l "$toi" "$f" > "$out.stdout" 2> "$out"
    kb=$(awk '/maximum resident set size/ {print int($1 / 1024)}' "$out")
    s=$(awk '/ real / {print $1}' "$out")
  else
    TOI_GC_STATS=1 /usr/bin/time -f 'time %e %M' "$toi" "$f" > "$out.stdout" 2> "$out"
    read -r _ s kb < <(grep '^time ' "$out" | tail -n 1)
  fi
  want=$(sed -n 's|^// expect: ||p' "$f")
  got=$(tail -n 1 "$out.stdout")
  [ "$got" = "$want" ] && ok=ok || ok="got $got, want $want"
  awk -v f="$(basename "$f")" -v s="$s" -v mb="$((kb / 1024))" -v ok="$ok" '
    /^gc [0-9]+ / {
      ms = $4 + 0
      total += ms
      if ($3 == "Minor:") { minors++; if (ms > minmax) minmax = ms }
      else { majors++; if (ms > majmax) majmax = ms }
    }
    /^  live / { if ($2 + 0 > live) live = $2 + 0 }
    END {
      printf "%-16s %7.2f %6d %6d %5.1f%% %5d (%5.1f) %5d (%5.1f)  %s\n",
        f, s, mb, live, (s > 0 ? total / 10 / s : 0),
        minors, minmax, majors, majmax, ok
    }' "$out"
  rm -f "$out" "$out.stdout"
done
