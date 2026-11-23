// A `for await` over a sync iterator holds each step's result across
// the reads of its `done` and `value`: a result only it refers to, whose
// prototype is a proxy with a getter for its `get` trap, stays alive
// through the collections that getter causes, and the trap gets it as
// the receiver. (The loop runs in the jobs after the script's value is
// taken: a result freed shows as a crash, or the heap check failing.)
// expect: ran
var out = [];
var handler = { get get() {
  var junk = [];
  for (var i = 0; i < 2000; i++) junk.push({ i });
  return function (t, k, rcv) { return k === "done" ? rcv.n >= 3 : rcv.n; };
} };
var proto = new Proxy({}, handler);
var n = 0;
var iterable = { [Symbol.iterator]() { return { next() {
  var r = { n: n++ };
  for (var i = 0; i < 20; i++) r["p" + i] = { i };
  Object.setPrototypeOf(r, proto);
  return r;
} }; } };
(async function () {
  for await (var v of iterable) out.push(v);
  if (out.join() !== "0,1,2") throw new Error(out.join());
})();
"ran";
