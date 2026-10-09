// Regular expression literals: test, exec with groups, lastIndex with g.
// for: regexp (62M exec calls), pdfjs, typescript, code-load, mandreel
// expect: false,bob@site.com,bob,site,5,a12;a25;a38;,true,12.5 Tf
var re = /(\w+)@(\w+)\.com/;
var m = re.exec("mail bob@site.com now");
var g = /a./g, found = "";
var x;
while ((x = g.exec("a1 a2 a3")) !== null) found += x[0] + g.lastIndex + ";";
[re.test("nobody"), m[0], m[1], m[2], m.index, found, /^\s+|\s+$/.test("  x"), /[\d\.]+\sTf/.exec("12.5 Tf")[0]].join(",");
