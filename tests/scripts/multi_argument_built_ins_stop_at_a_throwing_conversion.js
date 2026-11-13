// Built-ins converting several arguments stop at the first whose
// conversion throws: the next one's isn't run.
// expect: first,caught first,caught first,caught first,caught

var out = [];
for (var f of [(x, y) => Math.atan2(x, y), (x, y) => String.fromCharCode(x, y), (x, y) => Math.max(x, y), (x, y) => 'abc'.slice(x, y)]) {
  var log = []; var x = { valueOf() { log.push('first'); throw 1; } }; var y = { valueOf() { log.push('second'); return 1; } };
  try { f(x, y); } catch (e) { log.push('caught'); } out.push(log.join(','));
}
out.join(' ');
