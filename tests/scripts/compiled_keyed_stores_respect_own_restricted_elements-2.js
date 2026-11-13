// A store into an array whose own elements are restricted (frozen,
// sealed or not extensible, an element with attributes, a read-only
// `length`) compiled for its exact shape, a named load before it: the
// shape's elements typed `Any` (`NO_ELEMENTS`) took the cases that check
// nothing (`set_keyed_nc_ia`, `nc_a`, `nc_n`) once the indexed-store
// protector no longer bailed every keyed store (#240, PR #280's Codex
// review). Sloppy and strict, in place and appended, against node.
// expect-json: "frozen append 3 {\"ok\":20000}\nfrozen sloppy [1,2,3] 3 [1,2,3] 3 {\"ok\":20000} {\"ok\":20000} \nfrozen append 3 {\"TypeError\":20000}\nfrozen strict [1,2,3] 3 [1,2,3] 3 {\"TypeError\":10000,\"ok\":10000} {\"TypeError\":20000} \nfrozen-d append 3 {\"ok\":20000}\nfrozen-d sloppy [1.5,2,3] 3 [1.5,2,3] 3 {\"ok\":20000} {\"ok\":20000} \nfrozen-d append 3 {\"TypeError\":20000}\nfrozen-d strict [1.5,2,3] 3 [1.5,2,3] 3 {\"TypeError\":10000,\"ok\":10000} {\"TypeError\":20000} \nfrozen-o append 3 {\"ok\":20000}\nfrozen-o sloppy [{},2,3] 3 [{},2,3] 3 {\"ok\":20000} {\"ok\":20000} \nfrozen-o append 3 {\"TypeError\":20000}\nfrozen-o strict [{},2,3] 3 [{},2,3] 3 {\"TypeError\":10000,\"ok\":10000} {\"TypeError\":20000} \nsealed append 3 {\"ok\":20000}\nsealed sloppy [9,null,9.5] 3 [9,null,9] 3 {\"ok\":20000} {\"ok\":20000} \nsealed append 3 {\"TypeError\":20000}\nsealed strict [9,null,9.5] 3 [9,null,9] 3 {\"ok\":20000} {\"ok\":13333,\"TypeError\":6667} \nnoext append 3 {\"ok\":20000}\nnoext sloppy [9,null,9.5] 3 [9,null,9] 3 {\"ok\":20000} {\"ok\":20000} \nnoext append 3 {\"TypeError\":20000}\nnoext strict [9,null,9.5] 3 [9,null,9] 3 {\"ok\":20000} {\"ok\":13333,\"TypeError\":6667} \nro-elem append 51 {\"ok\":48}\nro-elem sloppy [1,2,9.5] 3 [1,9,9] 3 {\"ok\":20000} {\"ok\":20000} \nro-elem append 51 {\"ok\":48}\nro-elem strict [1,2,9.5] 3 [1,9,9] 3 {\"TypeError\":5000,\"ok\":15000} {\"TypeError\":6667,\"ok\":13333} \nacc-elem append 51 {\"ok\":48}\nacc-elem sloppy [7,2,9.5] 3 [7,9,9] 3 {\"ok\":20000} {\"ok\":20000} 9\nacc-elem append 51 {\"ok\":48}\nacc-elem strict [7,2,9.5] 3 [7,9,9] 3 {\"ok\":20000} {\"ok\":20000} 9\nro-len append 3 {\"ok\":20000}\nro-len sloppy [9,2,9.5] 3 [9,9,9] 3 {\"ok\":20000} {\"ok\":20000} \nro-len append 3 {\"TypeError\":20000}\nro-len strict [9,2,9.5] 3 [9,9,9] 3 {\"ok\":20000} {\"ok\":20000} \nta-noext append 3 {\"ok\":20000}\nta-noext sloppy [9,0,9] 3 [9,9,9] 3 {\"ok\":20000} {\"ok\":20000} \nta-noext append 3 {\"ok\":20000}\nta-noext strict [9,0,9] 3 [9,9,9] 3 {\"ok\":20000} {\"ok\":20000} "

var out = [];
function mk(kind) {
  var a;
  switch (kind) {
    case "frozen": a = [1, 2, 3]; a.tag = 1; Object.freeze(a); break;
    case "frozen-d": a = [1.5, 2, 3]; a.tag = 1; Object.freeze(a); break;
    case "frozen-o": a = [{}, 2, 3]; a.tag = 1; Object.freeze(a); break;
    case "sealed": a = [1, , 3]; a.tag = 1; Object.seal(a); break;
    case "noext": a = [1, , 3]; a.tag = 1; Object.preventExtensions(a); break;
    case "ro-elem": a = [1, 2, 3]; a.tag = 1; Object.defineProperty(a, 0, { writable: false }); break;
    case "acc-elem": a = [1, 2, 3]; a.tag = 1; Object.defineProperty(a, 0, { get() { return 7; }, set(v) { a.log = v; } }); break;
    case "ro-len": a = [1, 2, 3]; a.tag = 1; Object.defineProperty(a, "length", { writable: false }); break;
    case "ta-noext": a = new Int16Array(3); a.tag = 1; Object.preventExtensions(a); break;
  }
  return a;
}
var kinds = ["frozen", "frozen-d", "frozen-o", "sealed", "noext", "ro-elem", "acc-elem", "ro-len", "ta-noext"];
for (var k of kinds) {
  for (var strict of [false, true]) {
    var f = strict
      ? new Function("a", "v", "i", '"use strict"; var t = a.tag; try { a[i] = v; return "ok"; } catch (e) { return e.constructor.name; }')
      : new Function("a", "v", "i", "var t = a.tag; a[i] = v; return 'ok';");
    var a = mk(k), plain = [1, 2, 3]; plain.tag = 1;
    var r, errs = {};
    for (var n = 0; n < 20000; n++) { r = f(n & 1 ? plain : a, n & 2 ? 9.5 : 9, n % 4); errs[r] = (errs[r] || 0) + 1; }
    f = strict
      ? new Function("a", "v", "i", '"use strict"; var t = a.tag; try { a[i] = v; return "ok"; } catch (e) { return e.constructor.name; }')
      : new Function("a", "v", "i", "var t = a.tag; a[i] = v; return 'ok';");
    var mono = mk(k), r2; var errs2 = {};
    for (var n = 0; n < 20000; n++) { r2 = f(mono, 9, n % 3); errs2[r2] = (errs2[r2] || 0) + 1; }
    f = strict
      ? new Function("a", "v", "i", '"use strict"; var t = a.tag; try { a[i] = v; return "ok"; } catch (e) { return e.constructor.name; }')
      : new Function("a", "v", "i", "var t = a.tag; a[i] = v; return 'ok';");
    var app = mk(k), errs3 = {};
    for (var n = 0; n < 20000; n++) { var r3 = f(app, 9, app.length); errs3[r3] = (errs3[r3] || 0) + 1; if (app.length > 50) break; }
    out.push(k + " append " + app.length + " " + JSON.stringify(errs3));
    out.push(k + (strict ? " strict " : " sloppy ") + JSON.stringify(Array.from(a)) + " " + a.length + " " + JSON.stringify(Array.from(mono)) + " " + mono.length + " " + JSON.stringify(errs) + " " + JSON.stringify(errs2) + " " + (mono.log === undefined ? "" : mono.log));
  }
}
out.join("\n");
