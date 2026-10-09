// Array.prototype methods beyond push/pop/indexOf.
// for: pdfjs (splice 469k), typescript (concat, shift, sort), code-load
// expect: 1-4,2-3,5-1-4-2-3-6-7-8,5,1,4,2,3,100-20-3,1-2-3-4-5,2-3,1-x-y-z-4-5,1,x-y-z-4-5,0-x-y-z-4-5,5-4-z-y-x-0,4,true,false,2-4-6,--1,1,1
var a = [5, 1, 4, 2, 3];
var r = [a.slice(1, 3).join("-"), a.slice(-2).join("-"), a.concat([6, 7], 8).join("-"), a.join(),
         [3, 20, 100].sort().join("-"), a.slice().sort(function (x, y) { return x - y; }).join("-")];
var b = [1, 2, 3, 4, 5];
var removed = b.splice(1, 2, "x", "y", "z");
r.push(removed.join("-"), b.join("-"), b.shift(), b.join("-"));
b.unshift(0);
r.push(b.join("-"), b.reverse().join("-"), b.lastIndexOf("x"), Array.isArray(b), Array.isArray("b"));
r.push([1, 2, 3].map(function (x) { return x * 2; }).join("-"), [null, undefined, 1].join("-"));
var c = [1, 2, 3];
c.length = 1;
r.push(c.length, c.join("-"));
r.join(",");
