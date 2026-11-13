// A string wrapper on a prototype chain gives its characters at its
// indices; `in` finds a function's, an array's or a wrapper's slot-less
// `length` and `name`, own or inherited.
// expect: b,true,true,c,,c,true,true,true,true,true,true,false

var o = Object.create(new String("abc")), d = Object.create(o);
function hot(p, n) { var s = ""; for (var i = 0; i < n; i++) s = p[i % 3]; return s; }
function f(a) {}
var r = [o[1], 1 in o, "length" in o, d[2], o[5], hot(o, 3000),
  "name" in f, "length" in f, "length" in Object.create(f), "length" in new String("ab"),
  "length" in Object.create([]), "name" in Object.create(Math.max), "length" in Object(Symbol())];
r.join();
