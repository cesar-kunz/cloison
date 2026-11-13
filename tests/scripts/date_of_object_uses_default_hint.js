// `new Date(o)` calls ToPrimitive on an object with no hint, "default"
// (#147); a date still gives its time value and a string still parses.
// expect: 5 default 7 9 3

var hint; var o = { [Symbol.toPrimitive](h) { hint = h; return 5; } };
var r = [new Date(o).getTime(), hint];
var d = new Date(7); d[Symbol.toPrimitive] = function () { throw 1; };
r.push(new Date(d).getTime(), new Date({ toString() { return "1970-01-01T00:00:00.009Z"; }, valueOf: undefined }).getTime());
r.push(new Date("1970-01-01T00:00:00.003Z").getTime());
r.join(" ");
