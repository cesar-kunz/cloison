// `push` stores through `[[Set]]`: a proxy on the prototype chain has its
// `set` trap take a new element, also in compiled code made before it
// became a prototype.
// expect: 1 2 2 0,1,5 false

function f(a, v) { a.push(v); return a.length; }
for (var i = 0; i < 3000; i++) f([], i);
var log = [];
var a = [];
Object.setPrototypeOf(a, new Proxy([], { set(t, k, v, r) { log.push(k); return true; } }));
var r = [f(a, 1), f(a, 2)];
a[5] = 3;
r.push(a.length, log.join(), a.hasOwnProperty(0));
r.join(" ");
