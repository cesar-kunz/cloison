// A lexer's loop: classify each character of a string by its code.
// features: loop, string, call
// expect: 250190000
function countTokens(src) {
  var words = 0, digits = 0, inWord = false;
  for (var i = 0; i < src.length; i++) {
    var c = src.charCodeAt(i);
    var isAlpha = (c >= 97 && c <= 122) || (c >= 65 && c <= 90) || c == 95;
    if (isAlpha) {
      if (!inWord) words++;
      inWord = true;
    } else {
      inWord = false;
      if (c >= 48 && c <= 57) digits++;
    }
  }
  return words * 1000 + digits;
}
function main() {
  var src = "";
  for (var i = 0; i < 50; i++) src += "var x" + i + " = foo(bar, 12) + baz_q; ";
  var s = 0;
  for (var r = 0; r < 1000; r++) s += countTokens(src);
  return s;
}
main();
