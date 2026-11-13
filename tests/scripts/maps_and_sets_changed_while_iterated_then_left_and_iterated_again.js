// Repeated deletions and insertions in a map and a set while they are
// iterated, the iterations left by `break`, a throw and short patterns,
// and iterated again after (cleared too). (As V8 and SpiderMonkey.)
// expect: 1.2.4.5.7.8.10.11.100.103.106.109 2.5.8.11.109.101.104.107.110.200.203.206 109.101.104.107.110.200.203.206.102.105.108.111 t50 0 2 13 0.2.4.6.8.10.50.52.54.56.58.60.100 109 0 101 z,1

var out = [], m = new Map(), s = new Set();
for (var i = 0; i < 12; i++) { m.set(i, i); s.add(i); }
for (var r = 0; r < 3; r++) {
  for (var [k] of m) { if (k % 3 === r) { m.delete(k); m.set(k + 100, r); } if (k > 105) break; }
  out.push([...m.keys()].join("."));
}
try { for (var x of s) { s.delete(x + 1); s.add(x + 50); if (x > 30) throw "t" + x; } } catch (e) { out.push(e); }
var [a, b] = s; out.push(a, b, s.size);
for (var x of s) { if (x % 2) s.delete(x); }
out.push([...s].join("."));
var [[k1, v1], [k2]] = m; out.push(k1, v1, k2);
m.clear(); for (var e of m) out.push("never"); m.set("z", 1); out.push([...m].join());
out.join(" ")
