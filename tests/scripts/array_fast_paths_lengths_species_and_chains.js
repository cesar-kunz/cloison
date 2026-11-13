// Array methods' fast paths: `indexOf` within the length it read, a
// subclass's `splice` giving one of its own (ArraySpeciesCreate), and a
// prototype chain longer than `Array.prototype`, `Object.prototype` seen.
// expect: -1 true 1 2,3 true 0 p

var r = [];
var a = [1, 2]; r.push(a.indexOf(3, { valueOf() { a.push(3); return 0; } }));
class A extends Array {} var x = new A(1, 2, 3); var rm = x.splice(0, 1); r.push(rm instanceof A, rm.length, x.join());
var b = [, 1]; var saved = Object.getPrototypeOf(Array.prototype); Object.setPrototypeOf(Array.prototype, { 0: "p" });
r.push(b.slice().hasOwnProperty(0), b.indexOf("p"), [].concat(b)[0]);
Object.setPrototypeOf(Array.prototype, saved);
r.join(" ");
