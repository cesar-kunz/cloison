// A recursion through compiled code and a native's callback, deeper than
// the native stack compiled calls may use, still calls natives: the
// interpreter makes the calls past that limit, and natives have stack of
// their own past it (ECharts' force layout, a step calling the next from
// its callback, threw a RangeError once its keyed stores compiled, #240).
// expect: 1200

var left, k = 0;
var o = { step: function (cb) { var v = [0, 0]; v[0] = Math.random(); v[1] = v[0] * 2; left--; cb(left <= 0); } };
function go() { o.step(function (stop) { k++; if (!stop) [0].forEach(function () { go(); }); }); }
for (var r = 0; r < 50; r++) { left = 5; go(); }
left = 1200; k = 0; go(); k;
