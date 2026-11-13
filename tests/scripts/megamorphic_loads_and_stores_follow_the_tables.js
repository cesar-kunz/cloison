// Megamorphic named loads and stores (`get_named_rt`, `set_named_rt`,
// `set_named_rt_i`: sites given up on, probing the runtime's tables):
// own slots in and out of the object, a prototype's while the prototype
// epoch holds (changed, deleted, made an accessor), values of every type
// into fields of every type, transitions running out of out-of-line room,
// int32s into int32, double and any fields, frozen objects and setters.
// expect: 241758250 60000 last0 79960 0 499500 4 1 999

function make(k) {
  var o = {};
  for (var j = 0; j < k % 7; j++) o["p" + j] = j;
  o["q" + (k % 11)] = k;
  o.x = k;
  o.s = "s" + k;
  if (k % 3 == 0) for (var j = 0; j < 12; j++) o["w" + j] = j; // out of the object
  return o;
}
function Proto() {}
Proto.prototype.x = 1000;
function getx(o) { return o.x; }
function sets(o, v) { o.x = v; }
function geto(o) { return o.w11; }
function seto(o, v) { o.w11 = v; }
function addf(o, v) { o.fresh = v; }
function seti(o, i) { o.n = i | 0; }
function loads() {
  var objs = [];
  for (var k = 0; k < 60; k++) objs.push(make(k));
  for (var k = 0; k < 20; k++) { var p = new Proto(); p["r" + k] = k; objs.push(p); }
  var sum = 0;
  for (var it = 0; it < 3000; it++) {
    for (var i = 0; i < objs.length; i++) {
      var o = objs[i];
      var v = getx(o);
      sum = (sum + (typeof v == "number" ? v : v.length)) | 0;
      if (it % 50 == 7) sets(o, it % 3 == 0 ? 1.5 : it % 3 == 1 ? "str" : it);
      if (it % 50 == 9) sets(o, it);
      var w = geto(o);
      if (w !== undefined) { sum = (sum + w) | 0; seto(o, w + 1); }
    }
    if (it == 1200) Proto.prototype.x = 2000; // the prototype epoch
    if (it == 1500) delete Proto.prototype.x;
    if (it == 1800) Object.defineProperty(Proto.prototype, "x", { get: function () { return 3; }, configurable: true });
    if (it == 2000) objs[5].x = { a: 1 };
  }
  return sum;
}
// Transitions through a megamorphic store, out-of-line room running out.
function transitions() {
  var fresh = [];
  for (var r = 0; r < 300; r++) {
    for (var k = 0; k < 40; k++) {
      var o = make(k);
      for (var j = 0; j < (k + r) % 5; j++) o["z" + j] = j;
      fresh.push(o);
    }
  }
  for (var it = 0; it < 3; it++) for (var i = 0; i < fresh.length; i++) addf(fresh[i], it == 2 ? "last" + (i % 3) : it);
  var n = 0;
  for (var i = 0; i < fresh.length; i++) n += fresh[i].fresh.length;
  return n + " " + fresh[3].fresh;
}
// Int32 stores into int32, double and any fields.
function ints() {
  var nums = [];
  for (var k = 0; k < 40; k++) { var o = make(k); o.n = k % 2 ? 0.5 : k % 4 ? "a" : 1; nums.push(o); }
  for (var it = 0; it < 2000; it++) for (var i = 0; i < nums.length; i++) seti(nums[i], it);
  var t = 0;
  for (var i = 0; i < nums.length; i++) t += nums[i].n;
  return t;
}
// Frozen and accessor receivers.
var log = 0;
function special() {
  var fr = [];
  for (var k = 0; k < 30; k++) fr.push(make(k));
  Object.freeze(fr[4]);
  Object.defineProperty(fr[6], "x", { set: function (v) { log += v; }, get: function () { return 1; } });
  var e = 0;
  for (var it = 0; it < 1000; it++) for (var i = 0; i < fr.length; i++) { try { sets(fr[i], it); } catch (x) { e++; } }
  return e + " " + log + " " + fr[4].x + " " + fr[6].x + " " + fr[0].x;
}
loads() + " " + transitions() + " " + ints() + " " + special();
