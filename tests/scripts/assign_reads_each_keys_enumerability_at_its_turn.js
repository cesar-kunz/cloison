// `Object.assign` takes the source's keys once, then reads each one's
// property at its turn (the review of #301): a setter of the target's
// run by an earlier key makes a later one enumerable (copied; it was
// left out, the enumerable keys taken at the start), not enumerable or
// deletes it (not copied), or adds a key (not copied). With a symbol
// among the source's keys, and after a delete, the same. The expected
// values are V8's and SpiderMonkey's.
// expect: 2 undefined undefined undefined|1 2 undefined undefined undefined|1 2 undefined undefined undefined|1

function source(kind) {
  var s = { a: 1 };
  Object.defineProperty(s, "b", { value: 2, enumerable: false, configurable: true });
  s.c = 3;
  s.d = 4;
  if (kind == 1) s[Symbol.iterator] = 1;
  if (kind == 2) { s.x = 0; delete s.x; }
  return s;
}
function run(kind) {
  var s = source(kind);
  var t = {
    set a(v) {
      Object.defineProperty(s, "b", { enumerable: true });
      Object.defineProperty(s, "c", { enumerable: false });
      delete s.d;
      s.e = 5;
    },
  };
  Object.assign(t, s);
  // (Unchanged, the keys not enumerable stay out.)
  var u = Object.assign({}, source(kind));
  return [t.b, t.c, t.d, t.e].map(String).join(" ") + "|" + Object.keys(u).join("").length / 3 + (u.b === undefined ? "" : "!");
}
[run(0), run(1), run(2)].join(" ");
