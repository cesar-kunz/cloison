// A property key with a lone surrogate is its own key, not U+FFFD's: two
// such keys are two properties, listed and found as they are.
// expect: 2 55296/56320  5 {"\ud800":1,"\udc00":2} 55357

var r = [], o = {}; o["\ud800"] = 1; o["\udc00"] = 2;
r.push(Object.keys(o).length, Object.keys(o).map(k => k.charCodeAt(0)).join("/"));
r.push(({ ["\ud800"]: 1 })["\udc00"], ({ "\ud800": 5 })["\ud800"], JSON.stringify(JSON.parse('{"\\ud800":1,"\\udc00":2}')));
var ks = []; for (var k in { "\ud83d": 1 }) ks.push(k.charCodeAt(0)); r.push(ks.join());
r.join(" ");
