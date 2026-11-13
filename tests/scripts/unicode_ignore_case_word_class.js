// With `i` and `u`, `\w` has the characters folding to its units (U+017F,
// U+212A), and `\W` and `[\W]` don't (cloison-fuzz BUGS.md #7).
// expect: false,false,false,true,true,true,false,true,false,true

[/[\W]/iu.test("\u212a"), /[\W]/iu.test("\u017f"), /\W/iu.test("\u212a"), /\w/iu.test("\u017f"), /[\w]/iu.test("\u212a"), /[\W]/i.test("\u212a"),
 /[\W]/iv.test("\u212a"), /\w/iv.test("\u017f"), /[^\w]/iu.test("\u212a"), /\b/iu.test("\u017f")].join();
