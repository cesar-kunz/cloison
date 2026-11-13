// A global `replace` collects every match's groups, unmatched ones
// included, before it builds the result; a replacement function may
// run the same pattern again. `indexOf` searches eight units at a time.
// expect: v1:k1; :k22; v33:k3,[k1|v1|1|0]; [k22|undefined|undefined|7]; [k3|v33|33|13],-y--y-stringz-y--y-undefined-y--y-string,<>a<>a<>a<>,abc,a$b.c,t,19,6,-1,9
var out = [], s = "k1=v1; k22=; k3=v33";
out.push(s.replace(/(\w+)=(\w*)/g, "$2:$1"));
out.push(s.replace(/(\w+)=(v(\d+))?/g, function (m, k, v, d, at) { return "[" + k + "|" + v + "|" + d + "|" + at + "]"; }));
var re = /x(y)?/g;
out.push("xyzxxy".replace(re, function (m, y) { return "xyxy".replace(re, "-$1-") + typeof y; }));
out.push("aaa".replace(/a*?/g, "<$&>"), "abc".replace(/q/g, "-"), "a.b.c".replace(".", "$$"), " t ".replace(/^\s*|\s*$/g, ""));
out.push("long haystack with needle inside, needle twice".indexOf("needle"), "abcabcabd".indexOf("abd", 2), "ab".indexOf("abc"), "aaaaaaaaab".lastIndexOf("b"));
out.join(",");
