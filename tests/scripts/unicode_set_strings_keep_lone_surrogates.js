// A `v` mode class's string with a lone surrogate (`\q{\uD800x}`)
// matches that surrogate, alone, not U+FFFD nor half of a pair.
// expect: true,false,true,false,false,true,true,true,true

var t = (re, s) => new RegExp(re, 'v').test(s);
[t('^[\\q{\\uD800x}]$', '\uD800x'), t('^[\\q{\\uD800x}]$', '�x'), t('^[\\q{x\\uDC00}]$', 'x\uDC00'),
 t('^[\\q{\\uD800}]$', '𐀀'), t('[\\q{\\uD800}]', '😀'), t('^[\\q{\\u{1F600}}]$', '😀'),
 t('^[\\q{\\uFFFD}]$', '�'), t('^[\\q{\\uD83D\\uDE00}]$', '😀'), t('^[\\q{\\uDC00\\uD800}]$', '\uDC00\uD800')].join();
