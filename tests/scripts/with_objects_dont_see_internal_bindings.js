// The front end's own variables (a direct `eval`'s home object for
// `super`, `new.target`'s, a private name's) are never looked up in a
// `with` statement's object: one with properties by their names doesn't
// shadow them, and a proxy's `has` doesn't see them.
// expect: home:1 proxy:3 0 newtarget-eval:function undefined newtarget:undefined private:5 arguments:4

var log = [];
function t(name, f) { try { log.push(name + ":" + f()); } catch (e) { log.push(name + ":" + e.name); } }
var o = { __proto__: { x: 1 }, m() { with ({ '%home': { __proto__: { x: 2 } } }) { return eval('super.x'); } } };
t("home", () => o.m());
var seen = [];
var p = new Proxy({}, { has(t, k) { seen.push(String(k)); return false; } });
var o2 = { __proto__: { x: 3 }, m() { with (p) { return eval('super.x'); } } };
t("proxy", () => o2.m() + " " + seen.filter(k => k[0] == "%").length);
function nt() { with ({ '%newtarget': 9 }) { return eval('new.target'); } }
t("newtarget-eval", () => typeof new nt() + " " + nt());
function nt2() { with ({ '%newtarget': 9 }) { return new.target; } }
t("newtarget", () => nt2());
t("private", () => { with ({ "#x": 1, "#x%p0": 2 }) { class C { #x = 5; m() { return this.#x } } return new C().m(); } });
t("arguments", () => (function () { with ({ arguments: 4 }) { return eval('arguments'); } })(1));
log.join(" ");
