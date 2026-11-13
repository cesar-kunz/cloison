// On a non-extensible global object, Annex B's function in a block of an
// indirect `eval`, a direct one or another script, by a name the global
// object lacks, makes no `var` and is no error (B.3.2.2, B.3.2.3:
// `fnDefinable` false, CanDeclareGlobalVar); it stays its block's, and
// no setter of the global's prototypes is called. By the name of a
// property the global has, the property gets it. A sloppy store to an
// undeclared name makes no global there either, nor one to a name of an
// inherited data property, writable or not (10.1.9.2). JSC's values,
// but for the setter (#157).
// expect: function undefined undefined undefined undefined function undefined false TypeError 1 1 false false

var r = [], had = 1;
Object.defineProperty(Object.prototype, "ps", { set(v) { r.push("setter"); }, configurable: true });
Object.preventExtensions(globalThis);
(0, eval)("{ function nn() {} r.push(typeof nn); } r.push(typeof nn);"); r.push(typeof nn);
eval("{ function nd() {} } r.push(typeof nd);");
$262.evalScript("{ function ns() {} } r.push(typeof ns);");
(0, eval)("{ function ps() {} }");
(0, eval)("{ function had() {} }"); r.push(typeof had);
zz = 1; r.push(typeof zz, Object.getOwnPropertyNames(globalThis).some(n => n[0] == "n"));
try { (0, eval)("var nv; { function nv() {} }"); } catch (e) { r.push(e.name); }
Object.prototype.inh = 1; Object.defineProperty(Object.prototype, "ro", { value: 1 });
inh = 2; ro = 2; r.push(inh, ro, Object.hasOwn(globalThis, "inh"), Object.hasOwn(globalThis, "ro"));
r.join(" ");
