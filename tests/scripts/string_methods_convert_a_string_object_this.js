// String methods apply ToString to a String object `this`, through its
// `toString`.
// expect: xy,MN,thrown,bc,ef

var s = new String('abc'); s.toString = function () { return 'xyz'; };
var t = new String('pq'); t[Symbol.toPrimitive] = function () { return 'mn'; };
var u = new String('abc'); u.toString = function () { throw 'thrown'; };
var e; try { String.prototype.indexOf.call(u, 'a'); } catch (x) { e = x; }
[String.prototype.slice.call(s, 0, 2), String.prototype.toUpperCase.call(t), e, 'abc'.slice(1), new String('def').slice(1)].join();
