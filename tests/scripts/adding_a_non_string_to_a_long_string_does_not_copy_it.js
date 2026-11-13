// `s + x` with `x` neither a string nor an int32 (a double, a boolean,
// `null`, an object, a date, a BigInt) joins `x`'s string to `s` as two
// strings are, in a cons string, rather than copying `s` (quadratic:
// 50,000 `s = s + 1.5` took 5 s): the strings, `ToPrimitive`'s order and
// a symbol's TypeError are what V8, SpiderMonkey, JavaScriptCore and
// QuickJS give.
// expect: 10586|falseNaN01.5D31234567890142nulltrue1e+210101,2,3o-7undefined|42123456789013D1.50NaNfalseundefined-7o1,2,31001e+21truenull|A2.5|2.5A|abba|xxxxxxxxxxxxxxxxxxxx0.25truenull0.25truenull0.25truenull0.25truenull0.25truenull|TypeError|TypeError

var out = [];
var d = new Date(0); d.toString = function () { return "D"; };
var vals = [1.5, -0, 0, 1e21, NaN, true, false, null, undefined, 42, -7, 12345678901, { toString: function () { return "o"; } },
  { valueOf: function () { return 3; }, toString: function () { return "no"; } }, [1, [2, 3]], d, 10n];
var s = "";
for (var i = 0; i < 3000; i++) { var v = vals[i % vals.length]; s = (i & 1) ? s + v : v + s; }
out.push(s.length, s.slice(0, 60), s.slice(-60));
var log = [];
var a = { valueOf: function () { log.push("a"); return "A"; } }, b = { valueOf: function () { log.push("b"); return 2.5; } };
out.push(a + b, b + a, log.join(""));
var c = "x".repeat(20); for (var j = 0; j < 5; j++) c = c + 0.25 + true + null;
out.push(c);
try { "s" + Symbol("q"); } catch (e) { out.push(e.constructor.name); }
try { Symbol("q") + 1.5; } catch (e) { out.push(e.constructor.name); }
out.join("|")
