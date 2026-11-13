// A string object's characters are read-only for objects inheriting from
// it too, however it becomes their prototype (#240).
// expect: b,false,b,false,w,TypeError

var o = Object.create(new String("abc")); o[1] = "z";
var a = [0]; Object.setPrototypeOf(a, new String("abc")); a[1] = "z"; a[3] = "w";
var e; try { (function () { "use strict"; o[0] = 1; })(); } catch (x) { e = x.constructor.name; }
[o[1], o.hasOwnProperty(1), a[1], a.hasOwnProperty(1), a[3], e].join();
