// Functions with `let` and `const` are compiled (`lda_hole`, and
// `check_hole`, which exits for the interpreter to throw where a binding is
// read in its dead zone: through a closure called early, in a `switch`'s
// other case, in a loop's body before its declaration), and their bindings
// keep their types (an int32 counter, an object, a double, a string).
// expect: 26008256 0,ReferenceError,2,ReferenceError,2000,ReferenceError,2002,ReferenceError,4000,ReferenceError,4002,ReferenceError,6000,ReferenceError,6002,ReferenceError,8000,ReferenceError,8002,ReferenceError,10000,ReferenceError,10002,ReferenceError,12000,ReferenceError,12002,ReferenceError,14000,ReferenceError,14002,ReferenceError,16000,ReferenceError,16002,ReferenceError,18000,ReferenceError,18002,ReferenceError 100015000 201600 1000049996

function sum(n) { let s = 0; for (let i = 0; i < n; i++) { const d = i * 2; s += d; } return s; }
function late(k) {
  // `x` read before its declaration when `k` is odd.
  function peek() { return x; }
  let r = 0;
  if (k & 1) { try { r = peek(); } catch (e) { r = e.constructor.name; } }
  let x = k;
  return r === 0 ? peek() : r;
}
function swtch(k) {
  switch (k & 3) {
    case 0: let a = 1; return a;
    case 1: try { return a; } catch (e) { return "TDZ"; }
    default: return k;
  }
}
function inner(n) {
  let t = 0;
  for (let i = 0; i < n; i++) {
    if (i == n - 1) { try { t += y; } catch (e) { t += 1000; } }
    let y = i;
    t += y;
  }
  return t;
}
function typed(n) {
  let o = null, f = 0.5, s = "";
  for (let i = 0; i < n; i++) { o = { i }; f += o.i * 0.5; s = "" + (i & 7); }
  return o.i + f + s;
}
function run() {
  let out = [];
  let a = 0, b = [], c = 0, d = 0, e = "";
  for (let k = 0; k < 20000; k++) {
    a += sum(k & 63);
    if (k % 2000 < 4) b.push(late(k));
    c += typeof swtch(k) == "string" ? 1 : swtch(k);
    if (k % 100 == 0) d += inner(k & 31);
    if (k == 19999) e = typed(k);
  }
  out.push(a, b.join(","), c, d, e);
  return out.join(" ");
}
run();
