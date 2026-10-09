// charCodeAt, charAt and String.fromCharCode, in and out of range.
// for: crypto, gbemu, pdfjs, typescript, regexp
// expect: 65,122,48,57,126,233,z|NaN|65|Hi!AB| '.5<CJQX_fmt{|c
var s = "Az09~é", out = "";
for (var i = 0; i < s.length; i++) out += s.charCodeAt(i) + ",";
out += s.charAt(1) + s.charAt(9) + "|" + s.charCodeAt(9) + "|" + s.charCodeAt() + "|";
out += String.fromCharCode(72, 105, 33) + String.fromCharCode() + String.fromCharCode(65.7, 65536 + 66);
var t = "";
for (var c = 32; c < 127; c += 7) t += String.fromCharCode(c);
out + "|" + t + "|" + "abc"[2];
