// Functions whose variables closures capture, compiled: closures writing
// them during calls (also changing their type), a captured parameter,
// closures outliving the frame (closed at its return), made in a loop, a
// getter writing one between two reads, recursion, and an exception
// through such a function.
// for: earley-boyer, typescript, code-load, pdfjs
// expect: 5226021550|501|502|100
function counter(start) {
  var n = start;
  function inc(k) { n += k; return n; }
  var s = 0;
  for (var i = 0; i < 1000; i++) {
    s += inc(1);
    s += n;
    if (i === 500) n = "x";
    if (typeof n === "string") n = n.length;
  }
  return [s, n, inc];
}
function makers(m) {
  var fs = [], last = 0;
  for (var i = 0; i < m; i++) fs.push(function () { return i + last; });
  last = 100;
  return fs;
}
function viaGetter() {
  var v = 1;
  var o = { get g() { v = 50; return 1; } };
  var a = v;
  var b = o.g;
  return a + b + v;
}
function rec(d) {
  var here = d;
  function peek() { return here; }
  if (d === 0) return peek();
  return peek() + rec(d - 1);
}
function thrower(t) {
  var x = t;
  var f = function () { if (x > 3) throw new Error("big " + x); return x; };
  return f() + x;
}
function bumper() {
  var i, hits = 0;
  function bump() { i += 10; hits++; }
  for (i = 0; i < 100; i++) if (i % 7 === 3) bump();
  return i * 1000 + hits;
}
function both() {
  var a = arguments, k = a.length;
  function get(j) { return a[j] * k; }
  return get(0) + get(a.length - 1);
}
// Entered in its loop (on-stack replacement) where `helper`, which only
// `g` reads, is dead: its entry must not clear it.
function osr() {
  var helper = function (x) { return x + 1; };
  var g = function (y) { return helper(y); };
  var s = 0;
  for (var i = 0; i < 100000; i++) s += g(i);
  return s;
}
// Closures of one code, each with its own upvalues, called directly
// through one site (bound to the code): each must see its own.
function makeAdder(k) { return function (x) { return x * 10 + k; }; }
function adders() {
  var fs = [makeAdder(1), makeAdder(2), makeAdder(3)], s = 0;
  for (var i = 0; i < 30000; i++) s += fs[i % 3](i) % 10;
  return s;
}
var total = osr() + adders(), last = null, errs = 0;
for (var r = 0; r < 300; r++) {
  var res = counter(r);
  total += res[0] + res[1];
  last = res[2];
  var fs = makers(3);
  total += fs[0]() + fs[2]();
  total += viaGetter() + rec(5) + bumper() + both(r, 2, 3);
  try { total += thrower(r % 6); } catch (e) { errs++; }
}
[total, last(1), last(1), errs].join("|");
