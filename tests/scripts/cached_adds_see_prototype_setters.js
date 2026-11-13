// A cached store that adds a property no longer applies once a prototype
// has gained a setter or a read-only property of that name (`vm::Ic`'s
// `epoch`).
// expect: ro,false,false,5,3000

var r = [];
function F() {} var a1 = new F(), a2 = new F();
function add(o, v) { o.q = v; }
add(a1, 1); Object.defineProperty(F.prototype, "q", { value: "ro" }); add(a2, 2); r.push(a2.q, a2.hasOwnProperty("q"));
function H(v) { this.z = v; } new H(1); var log = [];
Object.defineProperty(H.prototype, "z", { set: function (v) { log.push(v); } }); var h = new H(5); r.push(h.hasOwnProperty("z"), log.join());
function G() {} function addp(o, v) { o.p = v; } for (var i = 0; i < 3000; i++) addp(new G(), i);
var hit = 0; Object.defineProperty(G.prototype, "p", { set: function (v) { hit++; } }); for (var i = 0; i < 3000; i++) addp(new G(), i); r.push(hit);
r.join();
