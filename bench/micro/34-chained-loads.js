// Two- and three-step field load chains.
// features: loop, call, object, new
// expect: 1499139
function S(v) { this.value = v; }
function V(s) { this.level = s; }
function C(s, v1, v2) { this.rank = s; this.v1 = v1; this.v2 = v2; }
function main() {
  var s0 = new S(0), s3 = new S(3), s5 = new S(5);
  var c = new C(s3, new V(s0), new V(s5)), n = 0;
  for (var i = 0; i < 1000000; i++) {
    if (c.rank.value < c.v2.level.value) n++;
    if (c.v1.level.value < c.rank.value) n += 2;
    if ((i & 1023) == 0) { var t = c.v1; c.v1 = c.v2; c.v2 = t; }
  }
  return n;
}
main();
