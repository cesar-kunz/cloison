// A lone surrogate's mapping (`js::lex::LONE_SURROGATES`) is
// escaped from the private use characters it is among, and U+FDD0
// (the runtime's own names' mark) from those.
// expect: 2,2,true,56192,2,2,1,55296,2,3,10,true,3,2,3,function,2

var a = String.fromCodePoint(0xF0000), b = "\u{F0000}", l = "\uD800";
var o = {}; o[a] = 1; o[l] = 2;
var k = Object.keys(o);
var r = [a.length, b.length, a === b, b.charCodeAt(0), k.length, k[0].length, k[1].length, k[1].charCodeAt(0)];
r.push(({ "\uD800": 1, "\u{F0000}": 2 })["\u{F0000}"], Object.keys({ "\u{F0800}x": 1 })[0].length);
r.push(String.raw`\u{F0000}x`.length, /\u{F0000}/u.test(b), eval('"\\uD800\\u{F0800}"').length);
var p = { "﷐y": function () {} }; p["﷐x"] = 1; p[Symbol("x")] = 2;
r.push(Object.keys(p).length, Reflect.ownKeys(p).length, typeof p["﷐y"], p["﷐y"].name.length);
r.join(",");
