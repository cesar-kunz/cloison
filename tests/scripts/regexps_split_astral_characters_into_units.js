// Outside `u` mode a pattern is code units: a literal astral character
// is two atoms, its lead and trail, a quantifier repeating the trail,
// and two class members (#109).
// expect: true,true,true,"\ud83d",true,true,true,true,true,true,false,true,true SyntaxError

var J = JSON.stringify;
[/^😀?$/.test("\ud83d"), /^[😀]$/.test("\ude00"), /^😀{2}$/.test("😀\ude00"), J(/😀?/.exec("\ud83d")[0]), /^😀$/.test("😀"),
 /^\😀$/.test("😀"), /^\😀?$/.test("\ud83d"), /^[\😀]$/.test("\ude00"), /^[😀]{2}$/.test("😀"), /^[a-😀]$/.test("\ude00"),
 /^[^😀]$/.test("\ud83d"), /(?<=\ude00)x/.test("😀x"), /^😀+$/u.test("😀😀")].join()
 + (() => { try { new RegExp("[😀-😂]"); } catch (e) { return " " + e.name; } })();
