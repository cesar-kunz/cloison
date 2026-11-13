// Calling or constructing an object that can't be names it as `display`
// does, not by its heap address (cloison-fuzz BUGS.md #23).
// expect: false [object Object] is not a function / [object Object] is not a constructor

var ms = [];
for (const f of [Math, {}, Object.create(null), new Proxy({}, {}), JSON]) {
  try { f(); } catch (e) { ms.push(e.message); }
  try { new f(); } catch (e) { ms.push(e.message); }
}
ms.some((m) => m.includes("0x")) + " " + ms[0] + " / " + ms[1];
