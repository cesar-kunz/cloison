// Each run of a `with` statement has its own binding of its object:
// closures made in it keep theirs, in a loop, a generator or a script; and
// the binding isn't a property of the global object.
// expect: 0,1,2 a,b 0,1 0,1 false

var r = [];
var fs = []; for (var i = 0; i < 3; i++) { with ({ i }) fs.push(() => i); } r.push(fs.map(f => f()).join());
var gs = []; for (var o of [{ v: "a" }, { v: "b" }]) with (o) gs.push(function () { return v; }); r.push(gs.map(f => f()).join());
function* g() { for (var j = 0; j < 2; j++) with ({ j }) yield () => j; } r.push([...g()].map(f => f()).join());
function h() { var hs = []; for (var k = 0; k < 2; k++) with ({ k }) hs.push(() => k); return hs.map(f => f()).join(); } r.push(h());
r.push(Object.getOwnPropertyNames(globalThis).some(k => k[0] == "%"));
r.join(" ");
