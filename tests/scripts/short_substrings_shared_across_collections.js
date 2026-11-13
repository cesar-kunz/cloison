// Substrings equal to one made since the last collection began are that
// one (`Vm::short_string`), and a whole string's is the string. A short
// substring made, dropped, collected (the nursery's memory reused), then
// made again must be a new string, not the dead one the cache held: the
// cache is emptied as each collection starts.
// expect: 0,true,alphabeta

var src = "alpha beta gamma delta epsilon zeta eta theta", bad = 0, kept = [];
for (var r = 0; r < 300; r++) {
  var w = r % 8, s = src.indexOf(" ", w * 5) + 1, e = s + 4;
  src.substring(s, e);
  var junk = [];
  for (var j = 0; j < 30000; j++) junk.push({x: j, y: j, z: j});
  var t = src.substring(s, e);
  kept.push([t, src.slice(s, e)]);
  junk = [];
  for (var j = 0; j < 30000; j++) junk.push("" + j + "-" + j);
}
for (var k = 0; k < kept.length; k++)
  if (kept[k][0] !== kept[k][1] || src.indexOf(kept[k][0]) < 0) bad++;
bad + "," + (src.substring(0) === src) + "," + src.slice(0, 5) + src.substr(6, 4)
