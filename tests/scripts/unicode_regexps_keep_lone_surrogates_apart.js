// In `u` mode an escaped surrogate pair, `\ud83d\ude00`, is one atom, its
// code point, which a quantifier repeats whole; any other surrogate, a
// class's member, `\u{d83d}`, is a lone one, not half of a pair (#108).
// expect: false,true,true,["x"],false,false,false,true,true,true,false,true,false,true

var e = "\u{1F600}", J = JSON.stringify;
[/^[\ud83d]\ude00$/u.test(e), /^\ud83d\ude00?$/u.test(""), /^\ud83d\ude00{2}$/u.test(e + e), J(/x\ud83d\ude00?/u.exec("x")),
 /^[\ud800-\udbff][\udc00-\udfff]$/u.test(e), /^\ud83d[\udc00-\udfff]$/u.test(e), /^(?:\ud83d)\ude00$/u.test(e),
 /^\ud83d\ude00$/u.test(e), /^[\ud83d\ude00]$/u.test(e), /^\ud801\udc00+$/iu.test("\u{10428}\u{10400}"),
 /^\u{d83d}\u{de00}$/u.test(e), /^\u{d83d}$/u.test("\ud83d"), /^[\ud83d]\ude00$/v.test(e), /^\ud83d\ude00?$/v.test("")].join();
