// expect: 1,0,3,9 1,0,3,9 1,0,3,5 1,5

function* inner(n) { for (let k = 0; k < n; k++) yield k * 3; return n; }
function run(g, resume) {
  const it = g(), out = [];
  try {
    for (let i = 0; i < 6; i++) {
      const r = i === 0 ? it.next() : i === 1 ? resume(it) : it.next();
      out.push(r.value);
      if (r.done) break;
    }
  } catch (e) { out.push("threw " + e); }
  return out.join(",");
}
[
  run(function* () { try { yield 1; } catch (e) { } finally { yield* inner(2); } return 9; }, (it) => it.throw(new RangeError("x"))),
  run(function* () { try { yield 1; } catch (e) { yield* inner(2); } return 9; }, (it) => it.throw(new RangeError("x"))),
  run(function* () { try { yield 1; } finally { yield* inner(2); } return 9; }, (it) => it.return(5)),
  run(function* () { try { yield 1; } finally { yield* inner(0); } return 9; }, (it) => it.return(5)),
].join(" ");
