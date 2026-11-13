// Conversions failing in hot functions' own `try` (the review of #190):
// every `valueOf`/`toString`/`@@toPrimitive` call is logged, so a redo
// that ran one again would show; `@@toPrimitive` throwing, and several
// conversions in one expression, the second one failing. The expected
// value is SpiderMonkey's and JSC's.
// expect: 6 1 c,c,c,c,c,c,c,c Av Bv Bt ETypeError P:default ERangeError Xv Y:number Zv Zt ETypeError M:number ERangeError Cv Dv Dt ETypeError T:number U:number ERangeError Gt Ht Hv ETypeError J:string K:string ERangeError

var log = [];
function mk(tag, mode) {
  return {
    valueOf() { log.push(tag + "v"); return mode === "obj" ? {} : 1; },
    toString() { log.push(tag + "t"); return mode === "obj" ? {} : "s"; }
  };
}
function tp(tag, thr) {
  return { [Symbol.toPrimitive](h) { log.push(tag + ":" + h); if (thr) throw new RangeError(tag); return 2; } };
}
function add(a, b) { try { return a + b; } catch (e) { log.push("E" + e.name); return "c"; } }
function mul3(a, b, c) { try { return a * b - c; } catch (e) { log.push("E" + e.name); return "c"; } }
function cmp(a, b) { try { return (a < b) + "|" + (a == b); } catch (e) { log.push("E" + e.name); return "c"; } }
function tmpl(a, b) { try { return `${a}-${b}`; } catch (e) { log.push("E" + e.name); return "c"; } }
var out = [];
for (var i = 0; i < 3000; i++) {
  var bad = i % 500 === 499;
  log.length = 0;
  var r = [
    add(bad ? mk("A") : 1, bad ? mk("B", "obj") : 2),
    add(bad ? tp("P", true) : 1, bad ? mk("Q") : 2),
    mul3(bad ? mk("X") : 3, bad ? tp("Y", false) : 4, bad ? mk("Z", "obj") : 5),
    mul3(bad ? tp("M", true) : 3, bad ? mk("N") : 4, 1),
    cmp(bad ? mk("C") : 1, bad ? mk("D", "obj") : 2),
    cmp(bad ? tp("T", false) : 1, bad ? tp("U", true) : 2),
    tmpl(bad ? mk("G") : 1, bad ? mk("H", "obj") : 2),
    tmpl(bad ? tp("J", false) : 1, bad ? tp("K", true) : 2)
  ];
  if (bad) out.push(r.join(",") + " " + log.join(" "));
}
out.length + " " + new Set(out).size + " " + out[0];
