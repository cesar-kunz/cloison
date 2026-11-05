#!/usr/bin/env bash
# Run bench/strings/concat.js on cloison (and the engines named), with
# what it allocated, its collections and its peak RSS for cloison.
#
#   bench/strings/concat.sh [ENGINE ...]    # ENGINE: a path, e.g. ~/.jsvu/bin/v8
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cloison=${CLOISON:-$root/target/release/cloison}
for e in "$cloison" "$@"; do
  echo "== $e"
  if [ "$e" = "$cloison" ]; then
    out=$( { CLOISON_SIZES=1 /usr/bin/time -f 'peak_rss_kib=%M' "$e" "$here/concat.js"; } 2>&1 )
    grep -v '^gc \|^jit \|peak_rss' <<<"$out" | grep -v '^undefined$' || true
    { grep '^gc ' <<<"$out" | grep -owE 'minors=[0-9]+|fulls=[0-9]+|heap_peak=[0-9]+|allocated=[0-9]+'; grep -oE 'peak_rss_kib=[0-9]+' <<<"$out"; } | tr '\n' ' '
    echo
  else
    "$e" "$here/concat.js"
  fi
done
