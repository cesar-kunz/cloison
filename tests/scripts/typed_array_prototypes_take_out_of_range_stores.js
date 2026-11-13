// Promoting a typed array to a prototype must not make a vector of every
// index (#240), yet it protects stores: an inherited store of an index
// outside its length does nothing (TypedArray [[Set]]).
// expect: false,2,false,0,false,5,3,false,false

var p = new Uint8Array(1 << 24);
var o = Object.create(p);
o[1 << 24] = 1; o[0] = 2; o[1e9] = 3;
var a = []; Object.setPrototypeOf(a, Object.create(new Int16Array(2)));
a[2] = 4; a[1] = 5; Array.prototype.push.call(a, 6);
(function () { "use strict"; o[5e7] = 7; })();
[o.hasOwnProperty(1 << 24), o[0], o.hasOwnProperty(1e9), p[0],
 a.hasOwnProperty(2), a[1], a.length, a.hasOwnProperty(0), o.hasOwnProperty(5e7)].join();
