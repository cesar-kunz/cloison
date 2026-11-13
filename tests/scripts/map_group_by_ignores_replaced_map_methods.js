// `Map.groupBy` makes its map's data directly: `Map.prototype.get` and
// `set` replaced aren't called.
// expect: ok,0,1:0,1:3,2:0:0,false

var calls = [];
var oset = Map.prototype.set, oget = Map.prototype.get;
Map.prototype.set = function () { calls.push('set'); throw 'overridden set'; };
Map.prototype.get = function () { calls.push('get'); throw 'overridden get'; };
var m, r;
try { m = Map.groupBy([1, 2, 3, -0, 0], function (x) { return x % 2; }); r = 'ok'; } catch (e) { r = String(e); }
Map.prototype.set = oset; Map.prototype.get = oget;
[r, calls.length, Array.from(m.keys()).join(':'), m.get(1).join(':'), m.get(0).join(':'), Object.is(Array.from(m.keys())[1], -0)].join();
