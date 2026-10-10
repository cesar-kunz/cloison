// GC: a table of 1,500,000 live records of mixed sizes, each replaced at
// random, over and over, with short-lived temporaries in between. The
// live data stays the same size throughout (about 170 MiB).
// gc: the replaced records die one by one all over the old generation,
// and a collector that never moves a cell leaves its blocks partly full,
// so the heap peaks at 4.6 times what is live (Splay's peak). The stores
// into the table dirty all of its cards, so minor pauses reach 20 ms.
// features: loop, call, object, new, array
// expect: 54233

var seed = 12345;
function rand(n) {
  seed = (seed * 16807) % 2147483647;
  return seed % n;
}

function Small(k) { this.k = k; this.v = null; }
function Medium(k) { this.k = k; this.a = k; this.b = k; this.c = k; this.d = k; this.e = k; this.v = null; }
function make(k) {
  switch (k % 3) {
    case 0: return new Small(k);
    case 1: return new Medium(k);
    default: var a = new Array(12); for (var i = 0; i < 12; i++) a[i] = k; return a;
  }
}

function key(x) { return x.k !== undefined ? x.k : x[0]; }

function run(live, rounds) {
  var table = new Array(live);
  for (var i = 0; i < live; i++) table[i] = make(i);
  var sum = 0;
  for (var r = 0; r < rounds; r++) {
    for (var i = 0; i < live; i++) {
      var j = rand(live);
      var x = table[j];
      sum = (sum + key(x)) % 1000003;
      table[j] = make(r * live + i);
      // Temporaries between the replacements, so the records that
      // survive a nursery are spread over its blocks.
      for (var t = 0; t < 3; t++) sum = (sum + key(make(t))) % 1000003;
    }
  }
  return sum;
}

run(1500000, 3);
