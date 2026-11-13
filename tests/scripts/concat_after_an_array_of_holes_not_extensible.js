// `concat` copies a dense array whole only where the result's elements
// end: after a frozen, sealed or non-extensible array of holes, copied
// element by element, what followed went to the elements' end
// (cloison-fuzz BUGS.md #21).
// expect: _,_,_,7 0,_,_,_,9 _,_,8,_,6 _,_,5

function show(a) { var r = []; for (var k = 0; k < a.length; k++) r.push(k in a ? String(a[k]) : "_"); return r.join(","); }
var f = Object.freeze(new Array(3)), s = Object.seal(new Array(2)), p = Object.preventExtensions(new Array(1));
[show(f.concat([7])), show([0].concat(f, [9])), show(s.concat([8], p, [6])), show(new Array(2).concat([5]))].join(" ");
