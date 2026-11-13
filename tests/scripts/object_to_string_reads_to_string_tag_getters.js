// `Object.prototype.toString` reads `Symbol.toStringTag` through its
// getter wherever it runs (a bytecode one, a native that writes: an
// operator's conversion redone), and throws what the getter throws. The
// expected value is SpiderMonkey's, JSC's and QuickJS's.
// expect: [object Custom],0,[object T],[object T],tag,tag,50000

var r = [], q = ["Custom"], p = {}, t = {}, th = {};
Object.defineProperty(p, Symbol.toStringTag, { get: Array.prototype.pop.bind(q) });
Object.defineProperty(t, Symbol.toStringTag, { get: function () { return "T"; } });
Object.defineProperty(th, Symbol.toStringTag, { get: function () { throw new Error("tag"); } });
r.push(p + "", q.length, t + "", String(t));
try { r.push(th + ""); } catch (e) { r.push(e.message); }
try { r.push(Object.prototype.toString.call(th)); } catch (e) { r.push(e.message); }
function g(n) { var s = 0; for (var i = 0; i < n; i++) s += (t + "").length; return s; }
r.push(g(5000));
r.join(",");
