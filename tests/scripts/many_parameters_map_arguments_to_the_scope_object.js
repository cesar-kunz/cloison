// A mapped `arguments` object kept in the scope object (a generator's,
// a direct `eval`'s function's, with a repeated name) of 130 parameters:
// the names reach `map_arguments_to_scope` in one register. One each
// took another 130 past the parameters' and the function was refused
// ("more than 256 registers", PR #215's review). What SpiderMonkey and
// JavaScriptCore give.
// expect: xyz xw q

var ps = [];
for (var i = 0; i < 130; i++) ps.push("p" + i);
var args = ps.map(function (_, i) { return i; });
var g = Function("return function* (" + ps.join(",") + ") { arguments[0] = 'x'; arguments[129] = 'y'; p1 = 'z'; yield p0 + p129 + arguments[1]; }")();
var e = Function("return function (" + ps.join(",") + ") { eval(''); arguments[0] = 'x'; p129 = 'w'; return p0 + arguments[129]; }")();
var d = Function("return function* (" + ps.join(",") + ",p0) { arguments[130] = 'q'; arguments[0] = 'n'; yield p0; }")();
[g.apply(null, args).next().value, e.apply(null, args), d.apply(null, args.concat([7])).next().value].join(" ")
