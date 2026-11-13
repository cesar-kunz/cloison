// `typeof` gives its name after the string was freed: its names are
// string constants, which a full marking that doesn't find them forgets
// (#33), but `Vm::type_names` kept them, so a later `typeof` gave the
// freed cell, by then another string (`typeof undefined` was "s74862"),
// or crashed. Rounds of collections, each name compared after.
// expect-json: "0 "

var bad = [];
for (var r = 0; r < 6; r++) {
  $262.gc(); $262.gc();
  for (var i = 0; i < 100000; i++) { var junk = { x: i, y: "s" + i }; }
  $262.gc();
  var names = [typeof undefined, typeof 1, typeof "", typeof {}, typeof true, typeof print];
  if (names.join() !== "undefined,number,string,object,boolean,function") bad.push(names.join());
  names = null;
}
bad.length + " " + bad.join(" ");
