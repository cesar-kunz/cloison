// A plain call's `this` is the global object (sloppy mode).
// for: navier-stokes (checkResult sets this.result)
// expect: 778false
function set(v) { this.result = v; }
function get() { return this.result; }
set(7);
var out = "" + result + get();
this.result = 8;
out + get() + (this === undefined);
