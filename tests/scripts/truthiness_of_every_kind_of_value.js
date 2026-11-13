// Truthiness in compiled code (`jtotrue`/`jtofalse`): objects of every
// class are true without the runtime, strings by their length. The
// expected value is Node's.
// expect: 3,3,30,3,3,3,3,3,30,30,30,30,3,30,30,3,3,3

var vals = [{}, [], "", "a", "a" + "b", function () {}, new Date(0), 0.5, -0, NaN, null, undefined, true, false, 0, 7, new ArrayBuffer(0), /x/];
function count(v) { var n = 0; for (var k = 0; k < 3; k++) { if (v) n++; if (!v) n += 10; } return n; }
var out = [];
for (var r = 0; r < 2000; r++) for (var i = 0; i < vals.length; i++) out[i] = count(vals[i]);
out.join();
