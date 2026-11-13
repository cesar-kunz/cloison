// `push` onto an array frozen, sealed, not extensible, of a read-only
// `length` (or sparse), compiled: the stencil leaves for the interpreter
// before changing anything, whose push throws the TypeError (the runtime
// call panicked on it), as V8, SpiderMonkey, JavaScriptCore and QuickJS.
// expect-json: "0:0:10,11:11\n1:3000:TypeError:3\n2:3000:TypeError:3\n3:3000:TypeError:3\n4:3000:TypeError:3\n5:0:108,109:109"

var out = [];
function mk(kind) {
  var a = [1, 2, 3]; a.push(4); a.pop();
  if (kind == 1) Object.freeze(a);
  if (kind == 2) Object.seal(a);
  if (kind == 3) Object.preventExtensions(a);
  if (kind == 4) Object.defineProperty(a, "length", { writable: false });
  if (kind == 5) a[100] = 9;
  return a;
}
function f(a, v) { return a.push(v); }
function g(a) { return a.push(1.5); }
for (var kind = 0; kind < 6; kind++) {
  var n = 0, r = "", a;
  for (var i = 0; i < 3000; i++) {
    a = (i % 7 == 0) ? mk(kind) : (a || mk(kind));
    try { r = f(a, i & 1 ? i : { k: i }) + "," + g(a); } catch (e) { n++; r = e.constructor.name; }
  }
  out.push(kind + ":" + n + ":" + r + ":" + a.length);
}
out.join("\n")
