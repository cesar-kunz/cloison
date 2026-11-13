// A hashbang comment, first thing in a script or `eval`'s code only.
// expect: 3,4,SyntaxError,SyntaxError,SyntaxError,SyntaxError

var r = [eval("#!x\n3"), eval("#!\u2028 4")];
for (var s of [" #!\n1", "1\n#!\n", "\\u0023!\n1"]) { try { eval(s); r.push("no"); } catch (e) { r.push(e.name); } }
try { Function("#!\n"); r.push("no"); } catch (e) { r.push(e.name); }
r.join();
