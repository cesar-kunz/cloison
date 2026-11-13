// A source's private use characters a lone surrogate's mapping uses
// (U+F0000 to U+F0800) and U+FDD0, written as they are, are themselves in
// strings, templates, patterns and `toString`, and don't shift a syntax
// error's column.
// expect: 7,f0000,fdd0,f0800,true,2,3,true,2
var s = "󰀀|﷐|󰠀"; function f() { return "󰀀" }
[s.length, s.codePointAt(0).toString(16), s.codePointAt(3).toString(16), s.codePointAt(5).toString(16),
 f.toString() === 'function f() { return "\u{F0000}" }', `󰀀`.length, String.raw`󰀀﷐`.length,
 /󰀀/u.test('\u{F0000}'), /󰠀/u.source.length].join();
