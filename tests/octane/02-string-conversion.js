// `String(v)` as a conversion, and `new String`'s absence of need.
// for: splay (String(key)), code-load
// expect: 12|0|1.5|true|null|undefined|s|1,2|[object Object]|string|
var out = [String(12), String(-0), String(1.5), String(true), String(null), String(undefined),
           String("s"), String([1, 2]), String({}), typeof String(3), String()];
out.join("|");
