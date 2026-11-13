// Calls with more arguments than registers (a callee's, `new`'s, a
// method's, `super`'s, one in `with`), templates with more pieces, and
// tagged ones: their lists in arrays or in parts (#73). (252 to 255
// arguments overflowed `Gen::alloc` in debug builds, more were refused;
// a script's 256th variable in a register overflowed too.)
// The expected value is JavaScriptCore's (V8's, SpiderMonkey's up to
// their limits on arguments).
// (A script with more variables in registers than there are, a
// block's: refused, not overflowing in debug builds.)
// expect: 251 252:251:false 252:251:true 252:251:true 252:251:true 252 251 254:251:false 252 252:251:true 253:251:true 254 255:254:false 255:254:true 255:254:true 255:254:true 255 254 257:254:false 255 255:254:true 256:254:true 255 256:255:false 256:255:true 256:255:true 256:255:true 256 255 258:255:false 256 256:255:true 257:255:true 299 300:299:false 300:299:true 300:299:true 300:299:true 300 299 302:299:false 300 300:299:true 301:299:true 69999 70000:69999:false 70000:69999:true 70000:69999:true 70000:69999:true 70000 69999 70002:69999:false 70000 70000:69999:true 70001:69999:true 251 252:251:false 252:251:true 252:251:true 252:251:true 252 251 254:251:false 252 252:251:true 253:251:true 254 255:254:false 255:254:true 255:254:true 255:254:true 255 254 257:254:false 255 255:254:true 256:254:true 255 256:255:false 256:255:true 256:255:true 256:255:true 256 255 258:255:false 256 256:255:true 257:255:true 299 300:299:false 300:299:true 300:299:true 300:299:true 300 299 302:299:false 300 300:299:true 301:299:true 69999 70000:69999:false 70000:69999:true 70000:69999:true 70000:69999:true 70000 69999 70002:69999:false 70000 70000:69999:true 70001:69999:true 251 252:251:false 252:251:true 252:251:true 252:251:true 252 251 254:251:false 252 252:251:true 253:251:true 254 255:254:false 255:254:true 255:254:true 255:254:true 255 254 257:254:false 255 255:254:true 256:254:true 255 256:255:false 256:255:true 256:255:true 256:255:true 256 255 258:255:false 256 256:255:true 257:255:true 299 300:299:false 300:299:true 300:299:true 300:299:true 300 299 302:299:false 300 300:299:true 301:299:true 69999 70000:69999:false 70000:69999:true 70000:69999:true 70000:69999:true 70000 69999 70002:69999:false 70000 70000:69999:true 70001:69999:true 602 eseses 600 TypeError true true true 301  \unicode a true/true/true

var out = [];
function zeros(n) { var a = []; for (var i = 0; i < n; i++) a.push(i); return a.join(","); }
function f() { return arguments.length + ":" + arguments[arguments.length - 1] + ":" + (this === o); }
function C() { this.n = arguments.length; this.l = arguments[arguments.length - 1]; }
var o = { f: f };
class A { m() { return arguments.length; } }
var g = Function("f", "o", "C", "A",
    "var r = [];" +
    "for (var i = 0; i < 3; i++) {" +
    [252, 255, 256, 300, 70000].map(function (n) {
        var a = zeros(n);
        return "r.push(Math.max(" + a + "), f(" + a + "), o.f(" + a + "), o['f'](" + a + "), o?.f(" + a + "), new C(" + a + ").n, new C(" + a + ").l," +
            " f(...[1, 2], " + a + "), new (class extends A { m() { return super.m(" + a + "); } })().m());" +
            "with (o) r.push(f(" + a + "), f(...[1], " + a + "));";
    }).join("") +
    "} return r.join(' ');");
out.push(g(f, o, C, A));
var log = [];
var v = { toString: function () { log.push("s"); return "x"; } };
function sub() { log.push("e"); return v; }
var t = Function("sub", "return `<" + "${sub()}-".repeat(300) + ">`;")(sub);
out.push(t.length, log.slice(0, 6).join(""), log.length);
try { Function("var s = 0; return `${s++}${Symbol()}" + "${s++}".repeat(300) + "`;")(); } catch (e) { out.push(e.name); }
function tag(s) { return s; }
var k = Function("tag", "return function () { return tag`\\unicode" + "${0}a".repeat(300) + "`; };")(tag);
var s1 = k(), s2 = k();
out.push(s1 === s2, Object.isFrozen(s1), Object.isFrozen(s1.raw), s1.length, s1[0], s1.raw[0], s1[300]);
// (A function a direct `eval` declared: called with no `this`, spread or not.)
out.push((function () { eval("var h = function () { 'use strict'; return this === undefined; }"); return [h(...[]), h(), h(0)].join("/"); })());
out.join(" ");
