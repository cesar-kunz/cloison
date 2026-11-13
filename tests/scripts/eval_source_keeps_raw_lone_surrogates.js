// `eval`'s and `Function`'s source is the string's code units: a raw
// lone surrogate in a regular expression literal is a pattern character,
// not the escape `\uD83D` (#122); in a string or template literal a code
// unit, which joins an escaped one into a pair, raw in `String.raw` and
// `toString`; in a comment nothing; in an identifier a SyntaxError.
// expect: false,true,true,SyntaxError,"\ud83d",false,true,1,true,true,true,1,true,7,true,2,2,2,SyntaxError,SyntaxError,true,3,2,false,1,17,false,1,1,true

var H = "\ud83d", L = "\ude00", P = "\u{1F600}", J = JSON.stringify;
function E(src) { var n; try { return eval(src); } catch (e) { n = e.name; } return n; }
function F(body) { var n; try { return Function(body)(); } catch (e) { n = e.name; } return n; }
[E("/" + H + "\\uDE00/u").test(P), E("/" + H + "\\uDE00/").test(P), E("/^\\" + H + "$/").test(H), E("/\\" + H + "/u"),
  J(E("/" + H + "/").source), E("/[" + H + "]/u").test(P), E("/" + H + L + "/u").test(P),
  E("'" + H + "'").length, E("'\\uD83D" + L + "'") === P, E("'" + H + "\\uDE00'") === P, E("'\\" + H + "'") === H,
  E("`" + H + "`").length, E("`" + H + "\\uDE00`") === P, E("String.raw`" + H + "\\uDE00`").length, E("(x=>x)`" + H + "`[0]") === H,
  E("String.raw`\\" + H + "`").length, E("1 /* " + H + " */ + 1"), E("1 // " + H + "\n + 1"), E("var a" + H + " = 1"), E(H),
  E("'\u{F0000}'").codePointAt(0) === 0xF0000, E("'\u{F0800}\u{FDD0}'").length, E("`\u{F0000}`").length,
  F("return /" + H + "\\uDE00/u.test('" + P + "')"), F("return '" + H + "'.length"),
  F("return (function(){ '" + H + "' }).toString().length"), (0, eval)("/" + H + "\\uDE00/u").test(P),
  E("({ '" + H + "': 1 })")[H], E("({ '\\uD83D" + L + "': 1 })")[P], E("/[\u{F0000}-\u{F0001}]/u").test("\u{F0001}")].join();
