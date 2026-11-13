// `shift` and `reverse` move a plain array's elements in place, and
// must barrier the slots they write: a young object moved into a slot
// on a clean card is otherwise missed by the next collection. Fails
// under `CLOISON_GC_CYCLES=1 CLOISON_GC_VERIFY=8` without the barriers.
// expect: 24000 24000

function run(move) {
  var keep = [];
  for (var r = 0; r < 200; r++) {
    var a = [];
    for (var i = 0; i < 16; i++) a.push({ id: i });
    move(a);
    keep.push(a);
    for (var j = 0; j < 50; j++) var junk = { a: j };
  }
  var sum = 0;
  for (var k = 0; k < keep.length; k++)
    for (var m = 0; m < keep[k].length; m++) sum += keep[k][m].id;
  return sum;
}
run(function (a) { a.shift(); }) + " " + run(function (a) { a.reverse(); });
