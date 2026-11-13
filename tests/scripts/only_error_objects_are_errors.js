// Only error objects are errors: not the objects of other classes whose
// flag shares `ERROR`'s bit (a resizable `ArrayBuffer`, a growable
// `SharedArrayBuffer`, a view tracking one's length, a far array, a
// function whose `name` was deleted), to `Error.isError` and the `stack`
// getter.
// expect: false/true,false/true,false/true,false/true,false/true,false/true,true/false,true/false

var sab = new SharedArrayBuffer(4, { maxByteLength: 8 });
var far = []; far[1e6] = 1;
var f = function g() {}; delete f.name;
var os = [new ArrayBuffer(1, { maxByteLength: 2 }), sab, new Int8Array(sab), new DataView(sab), far, f, new Error, new RangeError];
var stack = Object.getOwnPropertyDescriptor(Error.prototype, "stack");
os.map(o => Error.isError(o) + (stack ? "/" + (stack.get.call(o) === undefined) : "")).join();
