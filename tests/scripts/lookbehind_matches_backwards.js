// A lookbehind's body matches backwards, right to left: its greedy
// quantifiers and captures as the spec has them, and backreferences in it
// to groups on their right.
// expect: ["","1","053"] ["c","a"] ["c","a"] ["b","a"] ["b","a"] ["x","😀"] ["10.53",".53"] ["d"] ["y"]

function j(m) { return JSON.stringify(m); }
[j(/(?<=(\d+)(\d+))$/.exec("1053")), j(/(?<=(\w)+)c/.exec("abc")), j(/(?<=(a|b)*)c/.exec("abc")),
 j(/(?<=(a)\1)b/.exec("aab")), j(/(?<=\1(a))b/.exec("aab")), j(/(?<=(.))x/u.exec("\u{1F600}x")),
 j(/(?<=\$)\d+(\.\d*)?/.exec("cost $10.53")), j(/(?<=ab(?=c)\w)d/.exec("abcd")), j(/(?<=x{2,3})y/.exec("xxxy"))].join(" ");
