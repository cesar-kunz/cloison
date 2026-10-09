// Object.create, Object.keys, hasOwnProperty, Object.defineProperty with
// a getter and a setter on a prototype (box2d's b2ContactID.key).
// for: box2d, pdfjs, code-load
// expect: 42,42,hi x,true,false,a+b,true,false
function L() { this._key = 0; }
Object.defineProperty(L.prototype, "key", { enumerable: false, configurable: true,
  get: function () { return this._key; } });
Object.defineProperty(L.prototype, "key", { enumerable: false, configurable: true,
  set: function (v) { this._key = v * 2; } });
var l = new L();
l.key = 21;
var base = { greet: function () { return "hi " + this.n; } };
var d = Object.create(base);
d.n = "x";
var o = { a: 1, b: 2 };
[l._key, l.key, d.greet(), d.hasOwnProperty("n"), d.hasOwnProperty("greet"), Object.keys(o).join("+"),
 Object.getPrototypeOf(d) === base, Object.create(null) instanceof Object].join(",");
