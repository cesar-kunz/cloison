// Compiled `pop` leaves frozen, sealed and read-only-length arrays, and
// a hole at the end, to the interpreter (cloison-fuzz BUGS.md #9).
// expect: 200 3 200 3 200 3 200 x 495000

function pop(a) { return a.pop(); }
function errs(a) { var n = 0; for (var i = 0; i < 200; i++) { try { pop(a); } catch (e) { n++; } } return n; }
var fr = Object.freeze([1, 2, 3]), sl = Object.seal([1, 2, 3]), ro = [1, 2, 3];
Object.defineProperty(ro, "length", { writable: false });
var out = [errs(fr), fr.length, errs(sl), sl.length, errs(ro), ro.length, errs(Object.freeze([]))];
Array.prototype[2] = "x";
var h; for (var i = 0; i < 200; i++) h = pop([1, 2, , ]);
delete Array.prototype[2];
out.push(h);
var s = 0;
for (var k = 0; k < 100; k++) { var a = []; for (var j = 0; j < 100; j++) a.push(j); while (a.length) s += pop(a); }
out.push(s);
out.join(' ');
