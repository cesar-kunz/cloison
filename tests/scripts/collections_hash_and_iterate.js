// Maps and sets find keys through a hash index, by SameValueZero (1 and
// 1.0, -0 and +0, NaNs alike); iterations in progress go on past a
// compaction or a `clear`, seeing entries added after (`collections`).
// expect: 10000,100000000,false,true,3,b,z,z,n,n,false,1,2,,0/new,1/9,90/91/92/93/94/95/96/97/98/99/x,ba,2,100000,true,false,500,false,1,2,[[2,2]]

var r = [];
var m = new Map();
for (var i = 0; i < 20000; i++) m.set("k" + i, i);
for (var i = 0; i < 20000; i += 2) m.delete("k" + i);
var s = 0; for (var i = 1; i < 20000; i += 2) s += m.get("k" + i);
r.push(m.size, s, m.has("k2"), m.has("k3"));
var n = new Map([[1, "a"], [1.0, "b"], [-0, "z"], [NaN, "n"]]);
r.push(n.size, n.get(1), n.get(0), n.get(+0), n.get(NaN), n.get(0 / 0), n.has("1"));
var o1 = {}, o2 = {}; var w = new Map([[o1, 1], [o2, 2]]); r.push(w.get(o1), w.get(o2), w.get({}));
var st = new Set(); for (var i = 0; i < 40; i++) st.add(i);
var seen = []; for (var x of st) { seen.push(x); if (x === 0) { for (var j = 1; j < 40; j++) st.delete(j); st.add("new"); } }
r.push(seen.join("/"));
var st2 = new Set([1, 2, 3]); var seen2 = []; st2.forEach(function (x) { seen2.push(x); if (x == 1) { st2.clear(); st2.add(9); } }); r.push(seen2.join("/"));
var m3 = new Map(); for (var i = 0; i < 100; i++) m3.set(i, i); var it = m3.keys(); it.next(); it.next();
for (var i = 0; i < 90; i++) m3.delete(i); m3.set("x", 1); var rest = []; for (var k of it) rest.push(k); r.push(rest.join("/"));
var s4 = new Set(["a", "b"]); s4.delete("a"); s4.add("a"); r.push(Array.from(s4).join(""), s4.size);
var big = new Set(); for (var i = 0; i < 100000; i++) big.add(i * 1.5); r.push(big.size, big.has(3), big.has(1.25));
var wm = new WeakMap(); var ks = []; for (var i = 0; i < 1000; i++) { var k = {}; ks.push(k); wm.set(k, i); } r.push(wm.get(ks[500]), wm.has({}));
var sym = Symbol("s"); var ms = new Map([[sym, 1], ["s", 2]]); r.push(ms.get(sym), ms.get("s"));
var cl = new Map([[1,1]]); var ci = cl.entries(); cl.clear(); cl.set(2, 2); r.push(JSON.stringify(Array.from(ci)));
r.join();
