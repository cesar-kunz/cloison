// `{...o}` of a plain object with named data properties only reads its
// slots in order, no list of keys made; it copies what CopyDataProperties
// lists all the same: in order, enumerable ones only, onto a property the
// target has already (which keeps its place), and by the general walk
// for sources with a getter (called, in order), symbols, elements, or
// that are arrays, strings, proxies; a rest pattern leaves out its names.
// expect: a,b,c=1,2,3|a,b,c=1,9,3|b,a=5,1|a,c=1,3|a,g,z=1,g1,2|g>|a,s=1,sym|0,1,a=x,y,1|0,1=p,q|0,1,2=h,e,y|ownKeys,gopd:a,get:a|b,c=2,3|true,true,true|x=1|60000
var out = [];
function show(o) { var ks = Reflect.ownKeys(o); return ks.map(String).map(k => k.replace("Symbol(s)", "s")) + "=" + ks.map(k => o[k]); }
function copy(o) { return { ...o }; }
var p = { a: 1, b: 2, c: 3 };
out.push(show(copy(p)));
out.push(show({ a: 0, b: 0, ...p, b: 9 }));
out.push(show({ b: 5, ...{ a: 1 } }));
var hidden = { a: 1, b: 2, c: 3 };
Object.defineProperty(hidden, "b", { enumerable: false });
out.push(show(copy(hidden)));
var log = "";
var getter = { a: 1, get g() { log += "g>"; return "g" + this.a; }, z: 2 };
out.push(show(copy(getter)), log);
var s = Symbol("s");
out.push(show(copy({ a: 1, [s]: "sym" })));
out.push(show(copy({ a: 1, 1: "y", 0: "x" })));
out.push(show(copy(["p", "q"])), show(copy("hey")));
var trace = [];
var proxy = new Proxy({ a: 1 }, {
  ownKeys(t) { trace.push("ownKeys"); return Reflect.ownKeys(t); },
  getOwnPropertyDescriptor(t, k) { trace.push("gopd:" + k); return Reflect.getOwnPropertyDescriptor(t, k); },
  get(t, k) { trace.push("get:" + k); return t[k]; },
});
copy(proxy);
out.push(trace.join());
var { a, ...rest } = p;
out.push(show(rest));
// The copy's properties are plain data ones, whatever the source's were;
// null and undefined copy nothing.
var frozen = Object.freeze({ x: 1 });
var d = Object.getOwnPropertyDescriptor(copy(frozen), "x");
out.push([d.writable, d.enumerable, d.configurable].join());
out.push(show({ ...null, ...undefined, ...frozen }));
// Many of them, once compiled, with a source whose shape grows.
var t = 0;
for (var i = 0; i < 20000; i++) {
  var q = { a: 1, b: 2 };
  if (i & 1) q.extra = 2;
  var r = { ...q, a: 0 };
  t += r.a + r.b + (r.extra || 0);
}
out.push(t);
out.join("|");
