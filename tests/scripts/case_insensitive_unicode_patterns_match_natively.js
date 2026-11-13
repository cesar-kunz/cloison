// Case-insensitive Unicode patterns and `u` mode's code points in native
// code (#14): `i` with `u` or `v` compiled (it fell back to the
// interpreter), a unit past ASCII folding case and an astral code point
// tested by the runtime instead of bailing, and a class's lone surrogate
// an `Alone` unit instead of a lookahead and a lookbehind (which kept
// such patterns interpreted). Each pattern matched six times, past the
// execs that compile it: what V8, SpiderMonkey and JavaScriptCore give.
// expect: null ["\ud83d"] ["\ude00"] null null null null null null null null null null null null null ["é😀"] ["ab"] null null null null ["a","😀","b"] ["\ud83d"] ["\ude00"] ["x","\ude00","\ud83d"] ["\ud83d","x","\ude00"] ["é","😀"] ["a","b"] ["S","t","r","a","ß","e"," ","S","T","R","A","S","S","E"] ["𐐀","𐐨","x"] ["ſ","K","K","k"] ["Σ","σ","ς","Ω","ω","Ω"] ["😀","b"] ["\ud83d"] ["\ude00"] ["x","\ude00","\ud83d"] ["\ud83d","x","\ude00"] ["é","😀"] ["b"] ["S","t","r","ß","e"," ","S","T","R","A","S","S","E"] ["𐐀","𐐨","x"] ["ſ","K","K","k"] ["Σ","σ","ς","Ω","ω","Ω"] ["😀"] ["\ud83d"] ["\ude00"] ["\ude00","\ud83d"] ["\ud83d","\ude00"] ["é","😀"] null ["ß"," "] ["𐐀","𐐨"] ["ſ","K"] ["Σ","σ","ς","Ω","ω","Ω"] null ["\ud83d"] null ["\ud83d"] ["\ud83d"] null null null null null null ["a😀b"] ["\ud83d"] null ["x"] ["\ud83dx"] ["é😀"] ["ab"] ["Straße STRASSE"] ["𐐀𐐨x"] ["ſKKk"] ["ΣσςΩωΩ"] ["a","😀","b"] ["\ud83d"] ["\ude00"] ["x","\ude00","\ud83d"] ["\ud83d","x","\ude00"] ["é","😀"] ["a","b"] ["S","t","r","a","ß","e"," ","S","T","R","A","S","S","E"] ["𐐀","𐐨","x"] ["ſ","K","K","k"] ["Σ","σ","ς","Ω","ω","Ω"] ["a","😀","b"] ["\ud83d"] ["\ude00"] ["\ude00","\ud83d"] ["\ud83d","\ude00"] ["é","😀"] ["a","b"] ["S","t","r","a","ß","e"," ","S","T","R","A","S","S","E"] ["𐐀","𐐨"] ["ſ","K","K","k"] ["Σ","σ","ς","Ω","ω","Ω"] ["a","b"] null null ["x"] ["x"] null ["ab"] ["Straße","STRASSE"] ["x"] ["ſKKk"] null null null null null null null null null ["𐐀𐐨"] null null null null null null null null null null ["𐐀𐐨"] null null null null null null null null null ["S","S","SS"] null ["ſKKk"] null null null null null null null null ["S","S","SS"] null ["K","k"] null null null null null null null null null null null ["Σσς"] null null null null null null null null null null ["ΩωΩ"] null null null null null ["é"] null null null null null ["😀"] ["\ud83d"] ["\ude00"] ["\ude00\ud83d"] ["\ud83d","\ude00"] ["é😀"] null ["ß"," "] ["𐐀𐐨"] ["ſ","K"] ["ΣσςΩωΩ"] null null null null null null null ["ß","SS"] null null null null null null null null null null null null null null ["a","b"] null null ["x"] ["x"] ["é"] ["ab"] ["Straße","STRASSE"] ["𐐀𐐨x"] ["ſKKk"] ["ΣσςΩωΩ"] null null null null null ["é"] null ["ß"] ["𐐀𐐨"] null ["ΣσςΩωΩ"]

var out = [];
function T(re, s) { var r; for (var i = 0; i < 6; i++) { re.lastIndex = 0; r = JSON.stringify(s.match(re)); } return r; }
var subjects = ["a😀b", "\ud83d", "\ude00", "x\ude00\ud83d", "\ud83dx\ude00", "é😀", "ab", "Straße STRASSE", "\u{10400}\u{10428}x", "ſKKk", "ΣσςΩωΩ"];
var pats = [/^.$/u, /^..$/u, /./gu, /[^a]/gu, /\W/gu, /[\ud83d]/u, /[^\ude00]+/u, /./giv, /[^x]/giu,
  /[a-zß]+/giu, /\u{10428}+/giu, /[\u{10400}-\u{1044F}]+/giu, /[sk]+/giu, /[sk]+/gi, /σ+/giu, /[ω]+/giu, /[à-ÿ]+/gi, /[^a-z]+/gi, /ß|ss/giu,
  /([^\b]\|??.{0,2}?A?){1,}?(0{1,2}?)/iv, /\p{Lu}+/giu, /[\p{Ll}--[a-z]]+/giv];
for (var p of pats) for (var s of subjects) out.push(T(p, s));
out.join(" ");
