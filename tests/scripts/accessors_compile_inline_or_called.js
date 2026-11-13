// Getters and setters in prototypes, inlined or called from compiled code
// (`vm::ACCESSOR_LOC`): receivers of several shapes, a setter nested
// past the inlining depth, one writing its parameter, an assignment's
// value used after a setter, an own accessor shadowing, a redefinition.
// expect: 5041227

function Id() { this.f = { a: 0, b: 0 }; }
Object.defineProperty(Id.prototype, "key", { configurable: true, get: function () { return this._key; } });
Object.defineProperty(Id.prototype, "key", { configurable: true, set: function (v) {
  if (v === undefined) v = 0;
  this._key = v; this.f.a = v & 255; this.f.b = (v >> 8) & 255;
} });
function Holder() { this.id = new Id(); }
Holder.prototype.Set = function (o) { this.id.key = o.id.key; };
function Pair() { this.h = new Holder(); this.g = new Holder(); }
Pair.prototype.Copy = function (b) { this.h.Set(b.h); this.g.Set(b.g); };
function chain(x) { var y; y = x.key = 7; return y; }
function main() {
  var t = 0, ps = [], i;
  for (i = 0; i < 40; i++) { var p = new Pair(); p.h.id.key = i * 3; p.g.id.key = i * 257; ps.push(p); }
  for (var r = 0; r < 400; r++) {
    for (i = 1; i < ps.length; i++) {
      ps[i].Copy(ps[i - 1]);
      t += ps[i].h.id.key + ps[i].g.id.f.b;
    }
    ps[0].h.id.key = r; ps[0].g.id.key = r * 300;
    if (r == 200) ps[5].g.id.key = 1.5;         // a double through the setter
    if (r == 250) ps[6].h.id.key = undefined;   // the setter writes its parameter
    if (r == 300) { var s = new Id(); s.key = 5; s.extra = 1; ps[0].h.id = s; }   // another receiver shape
  }
  var o = new Id();
  t += chain(o) + o.key;
  // An own accessor shadowing the prototype's, then a redefinition.
  Object.defineProperty(ps[3].h.id, "key", { configurable: true, get: function () { return 1000; } });
  for (i = 0; i < 300; i++) t += ps[i % 40].h.id.key;
  Object.defineProperty(Id.prototype, "key", { configurable: true, get: function () { return -1; } });
  for (i = 0; i < 300; i++) t += ps[i % 40].h.id.key;
  return t;
}
main();
