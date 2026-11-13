// The new operators in code hot enough to compile.
// expect: 134540037 15471.988209 25005471.988209

function hot(o, i) {
  var s = 0;
  s += o?.a?.b ?? 1;
  s += o?.m?.(i) ?? 2;
  s += i ** 2;
  var q = o.c; q ??= 3; s += q;
  o.d ||= i; s += o.d;
  o.e &&= 5; s += o.e ?? 0;
  var { x = 1, ...r } = o.rest ?? {}; s += x + Object.keys(r).length;
  var sp = { ...o.rest, z: i }; s += sp.z;
  return s;
}
var total = 0;
for (var i = 0; i < 20000; i++) {
  var o = i % 3 == 0 ? { a: { b: i }, m(k) { return k + 1; }, c: null, e: 1, rest: { x: 2, y: 3 } }
        : i % 3 == 1 ? { a: null, c: 7, d: 4 } : { a: { b: null }, m: null, e: 0 };
  total += hot(o, i % 100);
}
var res = [total];
var t = 0; for (var j = 0; j < 10000; j++) { t += (j % 7) ** 0.5; } res.push(t.toFixed(6));
var u; for (var k = 0; k < 10000; k++) { u = k % 2 ? null : { v: k }; t += u?.v ?? -1; } res.push(t.toFixed(6));
res.join(" ");
