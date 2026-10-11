// `delete` in compiled code: own properties, missing ones, array
// elements, `length`, a global by name, and a prototype's method that the
// loop calls (compiled code relying on it must not call it after), with
// loads of the same objects around each delete.
// for: pdfjs, zlib, box2d, earley-boyer
// expect: 205424001|undefined|40000|0|false|number
function P() {}
P.prototype.m = function () { return 1; };
function run(n, Proto) {
  var s = 0;
  for (var i = 0; i < n; i++) {
    var o = { a: i, b: 2, c: 3 };
    s += o.a + o.b;
    if (delete o.b) s += 1;
    s += (o.b === undefined ? 10 : 0) + o.c;
    if (delete o.zz) s += 1;
    var arr = [1, 2, 3];
    delete arr[1];
    s += (1 in arr ? 100 : 0) + arr.length;
    if (!delete arr.length) s += 1;
    var p = new Proto();
    s += p.m ? p.m() : 1000;
    if (i === 15000) delete Proto.prototype.m;
  }
  return s;
}
function globals(n, G) {
  var c = 0;
  for (var i = 0; i < n; i++) {
    G.gg = i;
    if (delete gg) c++;
    if (typeof gg === "undefined") c++;
  }
  return c;
}
var r = [run(20000, P), typeof P.prototype.m, globals(20000, this)];
var q = { k: 1 };
delete q.k;
r.push(Object.keys(q).length, delete Math.PI, typeof Math.PI);
r.join("|");
