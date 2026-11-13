// ToNumber of a Date goes through its `valueOf` (or `@@toPrimitive`).
// expect: 42,0,8,5,9,3

var d = new Date(0); d.valueOf = function () { return 42; };
// (Date.prototype's `@@toPrimitive` is read-only: an assignment makes no
// own one, `defineProperty` does.)
var e = new Date(0); e[Symbol.toPrimitive] = function (h) { return h === 'number' ? 7 : 'x'; };
var f = new Date(0); Object.defineProperty(f, Symbol.toPrimitive, { value: function (h) { return h === 'number' ? 8 : 'y'; } });
[Math.abs(d), +e, +f, +new Date(5), new Date(9) - 0, Number(new Date(3))].join();
