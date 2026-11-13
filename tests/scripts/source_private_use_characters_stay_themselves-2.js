// A source's private use characters a lone surrogate's mapping uses
// (U+F0000 to U+F0800) and U+FDD0, written as they are, are themselves in
// strings, templates, patterns and `toString`, and don't shift a syntax
// error's column.
// compile error: 1:16: unexpected `x`
var x = "󰀀"; x x
