// A field typed `Num` holds doubles (`object::FieldType::Num`): ints
// stored before it was become doubles in the objects, stores convert
// ints (the interpreter's caches, also ones taken while it was `Int`, and
// the JIT's `_d` stores), and compiled loads take doubles unchecked.
// expect: 16501734,number,true,3

function V(x, y) { this.x = x; this.y = y; }
function W(x) { this.y = 0; this.x = x; }
function setx(o, v) { o.x = v; }
function sum(o) { return o.x + o.y; }
function poly(o, v) { o.x = v; return o.x; }
function main() {
  var vs = [];
  for (var i = 0; i < 200; i++) vs.push(new V(i, i + 1));
  var t = 0;
  for (var r = 0; r < 300; r++) {
    for (var i = 0; i < vs.length; i++) t += sum(vs[i]);
    if (r == 100) vs[3].x = 0.5;
    if (r == 150) for (var i = 0; i < vs.length; i += 7) setx(vs[i], i);
    if (r == 200) { delete vs[5].y; vs[5].y = 3; }
  }
  var ws = [new W(1), new W(2.5), new V(1, 2), new V(3, 4)];
  for (var r = 0; r < 2000; r++) t += poly(ws[r & 3], r & 7);
  for (var r = 0; r < 3000; r++) { var n = new V(r, 1); ws[r & 3] = n; t += n.x - n.y; }
  var u = new V(1, 2);
  u.x = 7;
  t += u.x * 3 + vs[3].x + vs[7].x;
  return [t, typeof vs[0].x, vs[14].x === 14, vs[5].y].join();
}
main();
