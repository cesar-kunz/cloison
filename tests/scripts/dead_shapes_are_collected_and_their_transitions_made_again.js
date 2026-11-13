// Shapes no object has die at full collections and become zombies
// (`Vm::sweep_shapes`): each round's constructor's root shape,
// transitions and prototype, most of a long literal's shapes, the copies
// a `delete` and `defineProperty` make. Live shapes' transitions to
// them are dropped, so taking them again makes new ones. Fails under
// `CLOISON_GC_VERIFY=2048` (with or without `CLOISON_GC_STW=1`) if an
// object reachable has a dead shape.
// expect: 807503

function make(k) {
  // A constructor of each round's own: its root shape, its transitions and
  // its prototype die with the round.
  function C(a) { this.a = a; this["p" + (k % 7)] = k; this.b = a * 2; }
  C.prototype.get = function () { return this.a + this.b; };
  return C;
}
function run() {
  var keep = [], shared = [], sum = 0;
  for (var round = 0; round < 120; round++) {
    var C = make(round), objs = [];
    for (var i = 0; i < 50000; i++) objs.push(new C(i));
    for (var i = 0; i < objs.length; i += 97) sum = (sum + objs[i].get()) % 1000003;
    // Plain objects whose shapes are made from a root that lives: the
    // transitions taken again after their shapes' objects died.
    var o = {};
    o.x = round;
    o["q" + (round % 5)] = 1;
    o.y = 2;
    shared.push(o);
    if (shared.length > 3) shared.shift();
    // A literal with many keys (one slot table), most of its shapes dead.
    if (round % 10 == 0) {
      var lit = {};
      for (var j = 0; j < 300; j++) lit["k" + j + "_" + round] = j;
      keep.push(lit);
    }
    // An object losing a property and changing an attribute.
    var d = { u: 1, v: 2, w: 3 };
    delete d.v;
    Object.defineProperty(d, "w", { enumerable: false });
    sum = (sum + d.u + d.w + Object.keys(d).length) % 1000003;
  }
  for (var i = 0; i < shared.length; i++) sum = (sum + shared[i].x + shared[i].y) % 1000003;
  for (var i = 0; i < keep.length; i++) sum = (sum + keep[i]["k5_" + i * 10]) % 1000003;
  return sum;
}
run();
