// String.prototype methods without regular expressions.
// for: typescript (indexOf, substring), pdfjs, code-load, gbemu
// expect: 4,8,8,-1,World,llo, ,Wor,World,llo, Wo,hello, world,HELLO, WORLD,pad|,4,a+b+c,Hello,Hello, World!1,true
var s = "Hello, World";
[s.indexOf("o"), s.indexOf("o", 5), s.lastIndexOf("o"), s.indexOf("zz"), s.substring(7), s.substring(7, 2),
 s.substr(-5, 3), s.slice(-5), s.slice(2, -3), s.toLowerCase(), s.toUpperCase(), "  pad  ".trim() + "|",
 "a,b,,c".split(",").length, "abc".split("").join("+"), s.split(", ", 1).join(), s.concat("!", 1),
 "ab".charAt(5) === ""].join(",");
