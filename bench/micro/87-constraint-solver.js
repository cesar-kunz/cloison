// Kernel: a whole incremental constraint solver in DeltaBlue's style, run on a chain and on a projection, repeatedly.
// cloison: the patterns of 1x-8x together, in phases whose type feedback differs (ints, then doubles; two constraint kinds, then four).
// features: loop, double, string, switch, call, object, new, proto, call-method, array, global, null, define-property
// expect: 8677475

// Classes: `extend` (on Object.prototype) links a constructor's prototype to its base's.
Object.defineProperty(Object.prototype, "extend", {
  value: function (base) {
    function Proto() {}
    Proto.prototype = base.prototype;
    this.prototype = new Proto();
    this.base = base;
  }
});

function Bag() { this.items = new Array(); }
Bag.prototype.add = function (x) { this.items.push(x); };
Bag.prototype.at = function (i) { return this.items[i]; };
Bag.prototype.size = function () { return this.items.length; };
Bag.prototype.take = function () { return this.items.pop(); };
Bag.prototype.remove = function (x) {
  var n = 0, gone = 0;
  for (var i = 0; i < this.items.length; i++) {
    var y = this.items[i];
    if (y != x) {
      this.items[n] = y;
      n++;
    } else {
      gone++;
    }
  }
  for (var i = 0; i < gone; i++) this.items.pop();
};

// Strengths: singletons compared by identity, ranked strongest (0) first.
function Level(rank, name) { this.rank = rank; this.name = name; }
Level.stronger = function (a, b) { return a.rank < b.rank; };
Level.weaker = function (a, b) { return a.rank > b.rank; };
Level.weakest = function (a, b) { return this.weaker(a, b) ? a : b; };
Level.prototype.next = function () {
  switch (this.rank) {
    case 0: return Level.STRONG;
    case 1: return Level.PREFERRED;
    case 2: return Level.NORMAL;
    case 3: return Level.WEAK;
    case 4: return Level.WEAKEST;
  }
  return null;
};
Level.REQUIRED = new Level(0, "required");
Level.STRONG = new Level(1, "strong");
Level.PREFERRED = new Level(2, "preferred");
Level.NORMAL = new Level(3, "normal");
Level.WEAK = new Level(4, "weak");
Level.WEAKEST = new Level(5, "weakest");

var Dir = new Object();
Dir.NONE = 0;
Dir.FWD = 1;
Dir.BACK = -1;

// A variable: its value, the rules on it, the rule that sets it.
function Var(name, value) {
  this.value = value || 0;
  this.rules = new Bag();
  this.by = null;
  this.mark = 0;
  this.walk = Level.WEAKEST;
  this.stay = true;
  this.name = name;
}
Var.prototype.attach = function (r) { this.rules.add(r); };
Var.prototype.detach = function (r) {
  this.rules.remove(r);
  if (this.by == r) this.by = null;
};

// Rules. Constructors add the rule to the graph and satisfy it.
function Rule(level) { this.level = level; }
Rule.prototype.enable = function () {
  this.attachAll();
  solver.add(this);
};
Rule.prototype.satisfy = function (mark) {
  this.choose(mark);
  if (!this.isSatisfied()) {
    if (this.level == Level.REQUIRED) solver.failures++;
    return null;
  }
  this.markInputs(mark);
  var out = this.output();
  var old = out.by;
  if (old != null) old.unsatisfy();
  out.by = this;
  if (!solver.propagate(this, mark)) solver.failures++;
  out.mark = mark;
  return old;
};
Rule.prototype.disable = function () {
  if (this.isSatisfied()) solver.remove(this);
  else this.detachAll();
};
Rule.prototype.isInput = function () { return false; };

function OneRule(v, level) {
  OneRule.base.call(this, level);
  this.out = v;
  this.sat = false;
  this.enable();
}
OneRule.extend(Rule);
OneRule.prototype.attachAll = function () {
  this.out.attach(this);
  this.sat = false;
};
OneRule.prototype.choose = function (mark) {
  this.sat = this.out.mark != mark && Level.stronger(this.level, this.out.walk);
};
OneRule.prototype.isSatisfied = function () { return this.sat; };
OneRule.prototype.markInputs = function (mark) {};
OneRule.prototype.output = function () { return this.out; };
OneRule.prototype.recalc = function () {
  this.out.walk = this.level;
  this.out.stay = !this.isInput();
  if (this.out.stay) this.run();
};
OneRule.prototype.unsatisfy = function () { this.sat = false; };
OneRule.prototype.inputsKnown = function (mark) { return true; };
OneRule.prototype.detachAll = function () {
  if (this.out != null) this.out.detach(this);
  this.sat = false;
};

// Keeps its variable as it is.
function Keep(v, level) { Keep.base.call(this, v, level); }
Keep.extend(OneRule);
Keep.prototype.run = function () {};

// Marks a variable the program is about to set.
function Input(v, level) { Input.base.call(this, v, level); }
Input.extend(OneRule);
Input.prototype.isInput = function () { return true; };
Input.prototype.run = function () {};

function TwoRule(a, b, level) {
  TwoRule.base.call(this, level);
  this.a = a;
  this.b = b;
  this.dir = Dir.NONE;
  this.enable();
}
TwoRule.extend(Rule);
TwoRule.prototype.choose = function (mark) {
  var am = this.a.mark == mark, bm = this.b.mark == mark;
  if (am && bm) this.dir = Dir.NONE;
  else if (am) this.dir = Level.stronger(this.level, this.b.walk) ? Dir.FWD : Dir.NONE;
  else if (bm) this.dir = Level.stronger(this.level, this.a.walk) ? Dir.BACK : Dir.NONE;
  else if (Level.weaker(this.a.walk, this.b.walk))
    this.dir = Level.stronger(this.level, this.a.walk) ? Dir.BACK : Dir.NONE;
  else this.dir = Level.stronger(this.level, this.b.walk) ? Dir.FWD : Dir.NONE;
};
TwoRule.prototype.attachAll = function () {
  this.a.attach(this);
  this.b.attach(this);
  this.dir = Dir.NONE;
};
TwoRule.prototype.isSatisfied = function () { return this.dir != Dir.NONE; };
TwoRule.prototype.markInputs = function (mark) { this.input().mark = mark; };
TwoRule.prototype.input = function () { return this.dir == Dir.FWD ? this.a : this.b; };
TwoRule.prototype.output = function () { return this.dir == Dir.FWD ? this.b : this.a; };
TwoRule.prototype.recalc = function () {
  var i = this.input(), o = this.output();
  o.walk = Level.weakest(this.level, i.walk);
  o.stay = i.stay;
  if (o.stay) this.run();
};
TwoRule.prototype.unsatisfy = function () { this.dir = Dir.NONE; };
TwoRule.prototype.inputsKnown = function (mark) {
  var i = this.input();
  return i.mark == mark || i.stay || i.by == null;
};
TwoRule.prototype.detachAll = function () {
  if (this.a != null) this.a.detach(this);
  if (this.b != null) this.b.detach(this);
  this.dir = Dir.NONE;
};

// a == b.
function Same(a, b, level) { Same.base.call(this, a, b, level); }
Same.extend(TwoRule);
Same.prototype.run = function () { this.output().value = this.input().value; };

// b == a * k + c, solved for either side; k and c are variables too.
function Linear(a, k, c, b, level) {
  this.dir = Dir.NONE;
  this.k = k;
  this.c = c;
  Linear.base.call(this, a, b, level);
}
Linear.extend(TwoRule);
Linear.prototype.attachAll = function () {
  Linear.base.prototype.attachAll.call(this);
  this.k.attach(this);
  this.c.attach(this);
};
Linear.prototype.detachAll = function () {
  Linear.base.prototype.detachAll.call(this);
  if (this.k != null) this.k.detach(this);
  if (this.c != null) this.c.detach(this);
};
Linear.prototype.markInputs = function (mark) {
  Linear.base.prototype.markInputs.call(this, mark);
  this.k.mark = this.c.mark = mark;
};
Linear.prototype.run = function () {
  if (this.dir == Dir.FWD) this.b.value = this.a.value * this.k.value + this.c.value;
  else this.a.value = (this.b.value - this.c.value) / this.k.value;
};
Linear.prototype.recalc = function () {
  var i = this.input(), o = this.output();
  o.walk = Level.weakest(this.level, i.walk);
  o.stay = i.stay && this.k.stay && this.c.stay;
  if (o.stay) this.run();
};

// The solver: adds and removes rules incrementally, and extracts plans.
function Solver() { this.mark = 0; this.failures = 0; }
Solver.prototype.newMark = function () { return ++this.mark; };
Solver.prototype.add = function (r) {
  var mark = this.newMark();
  var old = r.satisfy(mark);
  while (old != null) old = old.satisfy(mark);
};
Solver.prototype.remove = function (r) {
  var out = r.output();
  r.unsatisfy();
  r.detachAll();
  var left = this.unpropagate(out);
  var level = Level.REQUIRED;
  do {
    for (var i = 0; i < left.size(); i++) {
      var u = left.at(i);
      if (u.level == level) this.add(u);
    }
    level = level.next();
  } while (level != Level.WEAKEST);
};
Solver.prototype.plan = function (sources) {
  var mark = this.newMark();
  var steps = new Steps();
  var todo = sources;
  while (todo.size() > 0) {
    var r = todo.take();
    if (r.output().mark != mark && r.inputsKnown(mark)) {
      steps.add(r);
      r.output().mark = mark;
      this.consumers(r.output(), todo);
    }
  }
  return steps;
};
Solver.prototype.planFor = function (rules) {
  var sources = new Bag();
  for (var i = 0; i < rules.size(); i++) {
    var r = rules.at(i);
    if (r.isInput() && r.isSatisfied()) sources.add(r);
  }
  return this.plan(sources);
};
Solver.prototype.propagate = function (r, mark) {
  var todo = new Bag();
  todo.add(r);
  while (todo.size() > 0) {
    var d = todo.take();
    if (d.output().mark == mark) {
      this.remove(r);
      return false;
    }
    d.recalc();
    this.consumers(d.output(), todo);
  }
  return true;
};
Solver.prototype.unpropagate = function (out) {
  out.by = null;
  out.walk = Level.WEAKEST;
  out.stay = true;
  var left = new Bag(), todo = new Bag();
  todo.add(out);
  while (todo.size() > 0) {
    var v = todo.take();
    for (var i = 0; i < v.rules.size(); i++) {
      var r = v.rules.at(i);
      if (!r.isSatisfied()) left.add(r);
    }
    var by = v.by;
    for (var i = 0; i < v.rules.size(); i++) {
      var n = v.rules.at(i);
      if (n != by && n.isSatisfied()) {
        n.recalc();
        todo.add(n.output());
      }
    }
  }
  return left;
};
Solver.prototype.consumers = function (v, into) {
  var by = v.by, rs = v.rules;
  for (var i = 0; i < rs.size(); i++) {
    var r = rs.at(i);
    if (r != by && r.isSatisfied()) into.add(r);
  }
};

function Steps() { this.list = new Bag(); }
Steps.prototype.add = function (r) { this.list.add(r); };
Steps.prototype.size = function () { return this.list.size(); };
Steps.prototype.at = function (i) { return this.list.at(i); };
Steps.prototype.run = function () {
  for (var i = 0; i < this.size(); i++) this.at(i).run();
};

var solver = null;

// A chain of n equalities, kept at one end, edited at the other.
function chain(n) {
  solver = new Solver();
  var prev = null, first = null, last = null;
  for (var i = 0; i <= n; i++) {
    var v = new Var("v" + i);
    if (prev != null) new Same(prev, v, Level.REQUIRED);
    if (i == 0) first = v;
    if (i == n) last = v;
    prev = v;
  }
  new Keep(last, Level.NORMAL);
  var edit = new Input(first, Level.PREFERRED);
  var edits = new Bag();
  edits.add(edit);
  var steps = solver.planFor(edits);
  var ok = 0;
  for (var i = 0; i < 100; i++) {
    first.value = i;
    steps.run();
    if (last.value == i) ok++;
  }
  edit.disable();
  return ok + solver.mark + solver.failures * 1000000;
}

function change(v, x) {
  var edit = new Input(v, Level.PREFERRED);
  var edits = new Bag();
  edits.add(edit);
  var steps = solver.planFor(edits);
  for (var i = 0; i < 10; i++) {
    v.value = x;
    steps.run();
  }
  edit.disable();
}

// n pairs related by dst = src * scale + offset, changed from either side.
function projection(n) {
  solver = new Solver();
  var k = new Var("scale", 10), c = new Var("offset", 1000);
  var src = null, dst = null, dsts = new Bag();
  for (var i = 0; i < n; i++) {
    src = new Var("src" + i, i);
    dst = new Var("dst" + i, i);
    dsts.add(dst);
    new Keep(src, Level.WEAK);
    new Linear(src, k, c, dst, Level.REQUIRED);
  }
  var sum = 0;
  change(src, 17);
  sum += dst.value;
  change(dst, 1050);
  sum += src.value;
  change(k, 5);
  for (var i = 0; i < n - 1; i++) sum += dsts.at(i).value;
  change(c, 2000);
  for (var i = 0; i < n - 1; i++) sum += dsts.at(i).value;
  return sum + solver.mark + solver.failures * 1000000;
}

function main() {
  var total = 0;
  for (var r = 0; r < 25; r++) total += chain(100) + projection(100);
  return total;
}
main();
