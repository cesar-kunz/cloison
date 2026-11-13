// `push`'s Set of an index (OrdinarySet) calls a setter the array
// inherits, frozen or of a read-only `length` too, before its `length`
// fails; an inherited writable data property makes it on the array,
// which fails if it isn't extensible: what V8, SpiderMonkey,
// JavaScriptCore and QuickJS give, compiled or not.
// expect-json: "97781\n\n1\n2\n1\nTypeError\n1"

var out = [];
var log = "";
Object.defineProperty(Array.prototype, "1", { set(v) { log += "set(" + v + ") "; }, configurable: true });
var fr = Object.freeze([0]), pe = Object.preventExtensions([0]), ro = [0];
Object.defineProperty(ro, "length", { writable: false });
function h(x, v) { try { return x.push(v); } catch (e) { log += e.constructor.name + " "; } }
for (var j = 0; j < 2000; j++) { h(fr, j); h(pe, j); h(ro, j); if (j == 0 || j == 1999) log += "| "; }
out.push(log.length, log.slice(0, 120), fr.length, pe.length, ro.length);
delete Array.prototype[1];
Object.defineProperty(Array.prototype, "1", { value: 7, writable: true, configurable: true });
var pe2 = Object.preventExtensions([0]);
try { pe2.push(3); out.push("no throw"); } catch (e) { out.push(e.constructor.name, pe2.length); }
delete Array.prototype[1];
out.join("\n")
