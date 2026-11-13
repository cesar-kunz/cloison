// A function's slot-less `length` and `name`, and a string object's
// `length`, are read-only also inherited, and one redefined into a slot
// is listed once.
// expect: f 2 false 3 false TypeError "" 1 name 8 true

var r = [];
var o = Object.create(function f(a, b) {}); o.name = "x"; o.length = 7; r.push(o.name, o.length, o.hasOwnProperty("name"));
var s = Object.create(new String("abc")); s.length = 9; r.push(s.length, s.hasOwnProperty("length"));
r.push((function () { "use strict"; try { Object.create(function g() {}).name = 1; return "no"; } catch (e) { return e.name; } })());
function h() {} delete h.name; h.name = "x"; r.push(JSON.stringify(h.name));
var g = function () {}; Object.defineProperty(g, "name", { value: "z", enumerable: true });
var ks = []; for (var k in g) ks.push(k); r.push(Reflect.ownKeys(g).filter(k => k == "name").length, ks.join());
var g3 = function () {}; Object.defineProperty(g3, "length", { value: 5, writable: true });
var o3 = Object.create(g3); o3.length = 8; r.push(o3.length, o3.hasOwnProperty("length"));
r.join(" ");
