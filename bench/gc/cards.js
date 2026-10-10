// GC: an old table of 2,000,000 records, each of which is handed a new
// young object at a random position, about 100,000 stores per nursery.
// gc: every store from an old record to a young object dirties a card,
// and each minor collection scans every dirty card in full, 512 bytes of
// old records for the one young pointer in them: about 13 of a 16 ms
// pause is card scanning while tracing takes 2 ms (TypeScript's long
// minor pauses). The replaced children die old, so the heap peaks at
// three times what is live.
// features: loop, call, object, new, array
// expect: 573428

var seed = 12345;
function rand(n) {
  seed = (seed * 16807) % 2147483647;
  return seed % n;
}

function Node(k) { this.k = k; this.a = k; this.b = k; this.child = null; }
function Leaf(k) { this.k = k; }

function run(live, n) {
  var table = new Array(live);
  for (var i = 0; i < live; i++) table[i] = new Node(i);
  var sum = 0;
  for (var i = 0; i < n; i++) {
    var x = table[rand(live)];
    if (x.child !== null) sum = (sum + x.child.k) % 1000003;
    x.child = new Leaf(i);
    // Short-lived garbage between the stores, so nurseries fill and the
    // stores spread over all of them.
    for (var g = 0; g < 4; g++) sum = (sum + new Leaf(g).k) % 1000003;
  }
  return sum;
}

run(2000000, 5000000);
