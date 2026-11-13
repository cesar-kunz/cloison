// Strict code's store to a read-only global (`NaN`, a non-writable
// property of the global object) or a string object's character throws;
// sloppy code's does nothing.
// expect: TypeError TypeError TypeError TypeError ok ok ok ok 1 a y

function t(f) { try { f(); return "ok"; } catch (e) { return e.name; } }
Object.defineProperty(globalThis, "ro", { value: 1, writable: false });
var s = new String("ab");
var strict = (function () { "use strict"; return [t(() => { NaN = 1; }), t(() => { undefined = 1; }), t(() => { ro = 2; }), t(() => { s[0] = "x"; }), t(() => { s[5] = "y"; })]; })();
var sloppy = [t(() => { NaN = 1; }), t(() => { ro = 2; }), t(() => { s[0] = "z"; })];
strict.concat(sloppy, [ro, s[0], s[5]]).join(" ");
