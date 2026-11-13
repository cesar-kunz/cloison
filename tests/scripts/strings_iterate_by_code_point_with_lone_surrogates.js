// Strings iterate by code point; a lone surrogate, leading, trailing or
// high at the end, is one unit. (As V8 and SpiderMonkey.)
// expect: 1:97,1:98,1:55296,1:56320,2:55296,1:55296,120/56319,55357,

var out = [];
for (var c of "ab\ud800") out.push(c.length + ":" + c.charCodeAt(0));
for (var c of "\udc00𐀀\ud800") out.push(c.length + ":" + c.charCodeAt(0));
out.push([..."x\udbff"].map(c => c.charCodeAt(0)).join("/"));
var [h, t] = "\ud83d"; out.push(h.charCodeAt(0), t);
out.join()
