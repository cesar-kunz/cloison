// Recursion too deep throws, where a `catch` around it catches it: calls
// (`RangeError`), `JSON.parse`'s and `JSON.stringify`'s nesting
// (`RangeError`, as the native stack runs low), a pattern's groups and a
// program's nesting (`SyntaxError`, see `crate::stack`), rather than
// overflowing the native stack.
// expect: rec:caught RangeError,parse:RangeError,object:RangeError,stringify:RangeError,re:SyntaxError,re100:true,array:SyntaxError,parens:SyntaxError,functions:SyntaxError,negs:SyntaxError,blocks:SyntaxError,ifs:SyntaxError,parens50:1,functions20:1

var out = [];
function t(name, f) { try { out.push(name + ":" + f()); } catch (e) { out.push(name + ":" + e.name); } }
t("rec", function () { function f() { f(); } try { f(); } catch (e) { return "caught " + e.name; } });
t("parse", function () { JSON.parse("[".repeat(100000) + "]".repeat(100000)); return "ok"; });
t("object", function () { JSON.parse('{"a":'.repeat(100000) + "1" + "}".repeat(100000)); return "ok"; });
t("stringify", function () { var o = []; for (var i = 0; i < 100000; i++) o = [o]; JSON.stringify(o); return "ok"; });
t("re", function () { new RegExp("(".repeat(50000) + "a" + ")".repeat(50000)); return "ok"; });
t("re100", function () { return new RegExp("(".repeat(100) + "a" + ")".repeat(100)).test("a"); });
t("array", function () { eval("[".repeat(50000) + "]".repeat(50000)); return "ok"; });
t("parens", function () { return eval("(".repeat(50000) + "1" + ")".repeat(50000)); });
t("functions", function () { return eval("(function(){return ".repeat(5000) + "1" + ";})()".repeat(5000)); });
t("negs", function () { return eval("- ".repeat(100000) + "1"); });
t("blocks", function () { return eval("{".repeat(100000) + "}".repeat(100000)); });
t("ifs", function () { return eval("if(1)".repeat(100000) + "1;"); });
t("parens50", function () { return eval("(".repeat(50) + "1" + ")".repeat(50)); });
t("functions20", function () { return eval("(function(){return ".repeat(20) + "1" + ";})()".repeat(20)); });
out.join();
