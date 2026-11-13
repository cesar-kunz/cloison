// A pause traces on several threads past 16,384 cells
// (`Marking::drain_parallel`), the threads handing each other parts of
// their stacks: a tree and an array of 40,000 cells must survive. Fails
// under `CLOISON_GC_STW=1 CLOISON_GC_FULL=1 CLOISON_GC_VERIFY=2048` if a handed-over
// part is lost.
// expect: 490225

function Node(v, l, r) { this.v = v; this.l = l; this.r = r; }
function tree(d, v) { return d === 0 ? new Node(v, null, null) : new Node(v, tree(d - 1, 2 * v), tree(d - 1, 2 * v + 1)); }
function sum(t) { return t === null ? 0 : (t.v + sum(t.l) + sum(t.r)) % 1000003; }
function run() {
  var t = tree(14, 1), list = new Array(8000), junk = new Array(64), s = 0;
  for (var i = 0; i < 8000; i++) list[i] = new Node(i, null, null);
  for (var i = 0; i < 64; i++) junk[i] = list[0];
  for (var round = 0; round < 30; round++) {
    for (var g = 0; g < 3000; g++) junk[g & 63] = new Node(g, null, null);
    s = (s + sum(t)) % 1000003;
    for (var i = 0; i < 8000; i += 97) s = (s + list[i].v) % 1000003;
  }
  return s;
}
run();
