// A `finally` copied before a jump out of its `try` sees the loops
// around the `try`: a `continue` in it went to the inner loop the jump
// left, or for a `for`-`of` panicked in codegen (cloison-fuzz BUGS.md
// #22).
// expect: -39 -27 -27 0 9

function f(c) {
  var n = 0;
  for (const v of [1, 2, 3]) {
    try { for (const w of [1, 2]) { n += w; if (v === c) return n; } } finally { if (v === c) continue; }
    n += 10;
  }
  return -n;
}
function g(c) {
  var n = 0;
  for (let v = 1; v <= 3; v++) {
    try { for (let w = 1; w <= 2; w++) { n += w; if (v === c) return n; } } finally { if (v === c) continue; }
    n += 10;
  }
  return -n;
}
function* h() { for (let v = 0; v < 3; v++) { try { for (const w of [1, 2]) if (w === 2) return 9; } finally { if (v < 2) continue; } } }
[f(0), f(2), g(2), [...h()].length, h().next().value].join(" ");
