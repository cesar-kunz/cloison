// A function declared in a block of a direct `eval`'s code sets the
// function's `var` by its name, not a `with` object's property by it
// around the `eval` (B.3.2.3: the VariableEnvironment's
// SetMutableBinding), nor through a nested `eval`, a function of the
// code, or many bindings. SpiderMonkey's and JSC's values.
// expect: functionnumber functionnumber numbernumber functionnumber99 number

var log = [];
function t(f) { try { log.push(String(f())); } catch (e) { log.push(e.name); } }
var o = { f: 1 };
t(() => (function () { with (o) { eval('{ function f() {} }'); } return typeof f + typeof o.f; })());
t(() => (function () { with (o) { eval('eval("{ function f() {} }")'); } return typeof f + typeof o.f; })());
t(() => (function () { var f = 0; with (o) { eval('(function () { { function f() {} } return typeof f; })()'); } return typeof f + typeof o.f; })());
t(() => (function () { let z0 = 0;let z1 = 1;let z2 = 2;let z3 = 3;let z4 = 4;let z5 = 5;let z6 = 6;let z7 = 7;let z8 = 8;let z9 = 9;let z10 = 10;let z11 = 11;let z12 = 12;let z13 = 13;let z14 = 14;let z15 = 15;let z16 = 16;let z17 = 17;let z18 = 18;let z19 = 19;let z20 = 20;let z21 = 21;let z22 = 22;let z23 = 23;let z24 = 24;let z25 = 25;let z26 = 26;let z27 = 27;let z28 = 28;let z29 = 29;let z30 = 30;let z31 = 31;let z32 = 32;let z33 = 33;let z34 = 34;let z35 = 35;let z36 = 36;let z37 = 37;let z38 = 38;let z39 = 39;let z40 = 40;let z41 = 41;let z42 = 42;let z43 = 43;let z44 = 44;let z45 = 45;let z46 = 46;let z47 = 47;let z48 = 48;let z49 = 49;let z50 = 50;let z51 = 51;let z52 = 52;let z53 = 53;let z54 = 54;let z55 = 55;let z56 = 56;let z57 = 57;let z58 = 58;let z59 = 59;let z60 = 60;let z61 = 61;let z62 = 62;let z63 = 63;let z64 = 64;let z65 = 65;let z66 = 66;let z67 = 67;let z68 = 68;let z69 = 69;let z70 = 70;let z71 = 71;let z72 = 72;let z73 = 73;let z74 = 74;let z75 = 75;let z76 = 76;let z77 = 77;let z78 = 78;let z79 = 79;let z80 = 80;let z81 = 81;let z82 = 82;let z83 = 83;let z84 = 84;let z85 = 85;let z86 = 86;let z87 = 87;let z88 = 88;let z89 = 89;let z90 = 90;let z91 = 91;let z92 = 92;let z93 = 93;let z94 = 94;let z95 = 95;let z96 = 96;let z97 = 97;let z98 = 98;let z99 = 99; with (o) { eval('{ function f() {} }'); } return typeof f + typeof o.f + z99; })());
t(() => (function () { var s = ""; for (var i = 0; i < 3000; i++) with ({ f: 1 }) { eval('{ function f() {} }'); s = typeof f; } return s; })());
log.join(" ");
