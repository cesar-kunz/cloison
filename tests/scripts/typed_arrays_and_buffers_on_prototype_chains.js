// A typed array on a prototype chain gives its own elements at an index,
// and a buffer or a view none: their bytes aren't read as values.
// expect: 0 5 1.5 2.5  undefined undefined -3 4  12500

var r = [];
r.push(Object.create(new Uint8Array(16))[0]);
r.push(Object.create(new Uint8Array([5, 6, 7, 8, 9, 10, 11, 12]))[0]);
var f = Object.create(new Float64Array([1.5, 2.5])); r.push(f[0], f[1], f[2]);
r.push(typeof Object.create(new ArrayBuffer(16))[0], typeof Object.create(new DataView(new ArrayBuffer(16)))[0]);
var deep = Object.create(Object.create(new Int16Array([-3, 4]))); r.push(deep[0], deep[1], deep[5]);
var p = Object.create(new Float64Array([0.25, 0.5, 0.75, 1]));
function sum(o, n) { var s = 0; for (var k = 0; k < n; k++) for (var i = 0; i < 4; i++) s += o[i]; return s; }
r.push(sum(p, 5000));
r.join(" ");
