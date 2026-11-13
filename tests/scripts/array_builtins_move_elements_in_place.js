// `slice`, `splice`, `shift`, `unshift`, `concat` and `reverse` move
// elements in place (holes as they are), joining the elements' type once
// per value.
// expect: 3,4,5,6,7 5,6 0 2,3 1,4,5,6,7 0 1,4,x,y,5,6,7 6,7 1 object 5 1.5 4 9,8,2,3  5 1,2,3 false false 3 false 86887.5

var r = [];
var a = [1, 2, 3, 4, 5, 6, 7];
r.push(a.slice(2).join(), a.slice(-3, -1).join(), a.slice(5, 2).length);
var b = a.slice();
r.push(b.splice(1, 2).join(), b.join(), b.splice(2, 0, "x", "y").length, b.join());
r.push(b.splice(-2).join(), b.splice(0, 1, { k: 1 })[0], typeof b[0], b.length);
var c = [1.5, 2, 3];
r.push(c.shift(), c.unshift(9, 8), c.join(), [].shift());
r.push([1, 2].concat([3, [4]], 5).length, [3, 2, 1].reverse().join());
var h = [1, 2, 3]; delete h[1];
var g = [6, 7]; delete g[0];
r.push(1 in h.slice(), 3 in h.concat(g), h.reverse()[0], 1 in h);
var q = [], s = 0;
for (var i = 0; i < 300; i++) { q.unshift(i, i * 0.5); q.splice(q.length >> 1, 1); s += q[0]; }
while (q.length) s += q.shift();
r.push(s);
r.join(" ");
