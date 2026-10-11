// Keyed stores of named properties in compiled code (a table keyed by
// strings, past the point its stores went through the runtime): plain
// ones, one reaching a setter on the prototype (run once per store), the
// global object and a prototype as receivers (left to the interpreter),
// numeric strings on an array, and a read-only property.
// for: typescript, pdfjs, code-load
// expect: -39948|13333|1|5|19999|19999|25000|20000
function Table() { this.table = {}; }
Table.prototype.add = function (k, v) { this.table[k] = v; };
var sets = 0;
function P() {}
Object.defineProperty(P.prototype, "watched", { set: function (v) { sets += v; }, get: function () { return 0; } });
function Methods() {}
Methods.prototype.m = function () { return 1; };
var keys = [];
for (var k = 0; k < 40; k++) keys.push("k" + k);
var t = new Table(), p = new P(), arr = [], g = this, sum = 0, ro = 0;
for (var i = 0; i < 20000; i++) {
  t.add(keys[i % 40], i);
  t.add(keys[(i * 7) % 40], -i);
  p[i % 3 ? "watched" : "plain"] = 1;
  arr[String(i % 5)] = i;
  g["gk" + (i % 4)] = i;
  if (i === 15000) Methods.prototype["m"] = function () { return 2; };
  sum += new Methods().m();
  var s = "abc";
  s["x"] = 1;
  if (s.x === undefined) ro++;
}
var tsum = 0;
for (var k = 0; k < 40; k++) tsum += t.table[keys[k]];
[tsum, sets, p.plain, arr.length, arr[4], gk3, sum, ro].join("|");
