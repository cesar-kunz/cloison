// RegExp odds and ends: a global `replace` or `match` sets `lastIndex`
// with throw; a RegExp-like object makes a RegExp of its `source` and
// `flags`; Annex B's `\c` before a non-letter is a backslash, then `c`.
// expect: TypeError TypeError ba /x/g /x/i true true false true

function t(f) { try { return String(f()); } catch (e) { return e.name; } }
var ro = /a/g; Object.defineProperty(ro, "lastIndex", { writable: false, value: 0 });
var ro2 = /a/; Object.defineProperty(ro2, "lastIndex", { writable: false, value: 0 });
[t(() => "aa".replace(ro, "b")), t(() => "aa".match(ro)), t(() => "aa".replace(ro2, "b")),
 String(new RegExp({ source: "x", flags: "g", [Symbol.match]: true })), String(new RegExp({ source: "x", flags: "g", [Symbol.match]: true }, "i")),
 /\c1/.test("\\c1"), /a\c/.test("a\\c"), /^\c1$/.test("\\1"), /\cj/.test("\n")].join(" ");
