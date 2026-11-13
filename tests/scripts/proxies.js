// Proxies: each trap (and forwarding without one), callable and
// constructible proxies, proxies on prototype chains (also in code hot
// enough to compile), revocation, and the builtins listing keys through
// them.
// expect: get 10,20,missing,10 | set 6 | has true,false | delete false | keys b,c,virtual | forin b,c,virtual | json {"x":1,"y":[1,2]} | spread {"s":1} | calls get a;get b;get zzz;get a;set c;has a;has hidden;delete a;ownKeys;gopd b;gopd c;gopd virtual;ownKeys;gopd b;gopd c;gopd virtual | apply applied 5 function | construct {"made":7} | transparent 3,F,1 | proto true,true | isArray true,[object Array] | protoChain 1 from proxy inherited | protoSet x true | revocable 1 | revoked !TypeError revoked | revokedTypeof object | noNew !TypeError | badArgs !TypeError | defineTrap z:4 4 | trapReturnsFalse !TypeError | hot 3000 | hotProtoChain 15000 | reflect true,1,a,true,true,false | freeze true false

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
function t(name, f) { try { log(name, f()); } catch (e) { log(name, "!" + e.name + (String(e && e.message).indexOf("revoked") >= 0 ? " revoked" : "") + (e && e.name ? "" : " raw:" + String(e))); } }
var calls = [];
var target = { a: 1, b: 2 };
var handler = {
  get(t, k, r) { calls.push("get " + String(k)); return k in t ? t[k] * 10 : "missing"; },
  set(t, k, v, r) { calls.push("set " + k); t[k] = v + 1; return true; },
  has(t, k) { calls.push("has " + k); return k !== "hidden"; },
  deleteProperty(t, k) { calls.push("delete " + k); delete t[k]; return true; },
  ownKeys(t) { calls.push("ownKeys"); return Reflect.ownKeys(t).concat(["virtual"]); },
  getOwnPropertyDescriptor(t, k) { calls.push("gopd " + k); return k === "virtual" ? { value: 99, enumerable: true, configurable: true } : Reflect.getOwnPropertyDescriptor(t, k); },
};
var p = new Proxy(target, handler);
t("get", () => [p.a, p.b, p.zzz, p["a"]].join());
t("set", () => { p.c = 5; return target.c; });
t("has", () => ["a" in p, "hidden" in p].join());
t("delete", () => { delete p.a; return "a" in target; });
t("keys", () => Object.keys(p).join());
t("forin", () => { var ks = []; for (var k in p) ks.push(k); return ks.join(); });
t("json", () => JSON.stringify(new Proxy({ x: 1, y: [1, 2] }, {})));
t("spread", () => JSON.stringify({ ...new Proxy({ s: 1 }, {}) }));
t("calls", () => calls.join(";"));
var fp = new Proxy(function (a, b) { return a + b; }, { apply(t, th, args) { return "applied " + t(...args); } });
t("apply", () => fp(2, 3) + " " + typeof fp);
var cp = new Proxy(class { constructor(x) { this.x = x; } }, { construct(t, args, nt) { return { made: args[0] }; } });
t("construct", () => JSON.stringify(new cp(7)));
var plainFn = new Proxy(function F(x) { this.v = x; }, {});
t("transparent", () => [new plainFn(3).v, plainFn.name, plainFn.length].join());
t("proto", () => { var q = new Proxy({}, { getPrototypeOf() { return Array.prototype; } }); return [Object.getPrototypeOf(q) === Array.prototype, q instanceof Array].join(); });
t("isArray", () => [Array.isArray(new Proxy([], {})), Object.prototype.toString.call(new Proxy([], {}))].join());
t("protoChain", () => { var base = new Proxy({}, { get(t, k, r) { return "from proxy " + String(k); } }); var o = Object.create(base); o.own = 1; return o.own + " " + o.inherited; });
t("protoSet", () => { var log2 = []; var base = new Proxy({}, { set(t, k, v, r) { log2.push(k); return Reflect.set(t, k, v, r); } }); var o = Object.create(base); o.x = 1; return log2.join() + " " + o.hasOwnProperty("x"); });
var r = Proxy.revocable({ q: 1 }, {});
t("revocable", () => r.proxy.q);
r.revoke();
t("revoked", () => r.proxy.q);
t("revokedTypeof", () => typeof r.proxy);
t("noNew", () => Proxy({}, {}));
t("badArgs", () => new Proxy(1, {}));
t("defineTrap", () => { var seen; var q = new Proxy({}, { defineProperty(t, k, d) { seen = k + ":" + d.value; return Reflect.defineProperty(t, k, d); } }); Object.defineProperty(q, "z", { value: 4 }); return seen + " " + q.z; });
t("trapReturnsFalse", () => { "use strict"; var q = new Proxy({}, { set() { return false; } }); q.a = 1; return "no throw"; });
t("hot", () => { var q = new Proxy({ n: 0 }, { get(t, k) { return k == "n" ? t.n + 1 : undefined; } }); var s = 0; for (var i = 0; i < 3000; i++) s += q.n; return s; });
t("hotProtoChain", () => { var base = new Proxy({}, { get(t, k) { return 5; } }); var objs = []; for (var i = 0; i < 3; i++) objs.push(Object.create(base)); var s = 0; for (var j = 0; j < 3000; j++) s += objs[j % 3].x; return s; });
t("reflect", () => { var q = new Proxy({ a: 1 }, {}); return [Reflect.has(q, "a"), Reflect.get(q, "a"), Reflect.ownKeys(q).join(), Reflect.isExtensible(q), Reflect.preventExtensions(q), Object.isExtensible(q)].join(); });
t("freeze", () => { var q = new Proxy({ a: 1 }, {}); Object.freeze(q); return Object.isFrozen(q) + " " + Object.getOwnPropertyDescriptor(q, "a").writable; });
out.join(" | ");
