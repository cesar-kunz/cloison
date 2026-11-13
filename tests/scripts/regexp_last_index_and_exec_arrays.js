// `lastIndex` read and stored in its slot, `exec` arrays given their
// shape by a cache, literals found by search: as Node does.
// expect: 4,ab,b,6,4,b,6,1,3,,0,7,undefined,8,false,0,true,3,0|1|index|input|groups,true,2,0|1|index|input|groups|extra,0|1|2|index|input|groups,1,a1b,3,2,false,xx-ab,4
var out = [], re = /a(b)?/g, s = "xab ab a";
re.lastIndex = "2"; var m = re.exec(s); out.push(m.index, m[0], m[1], re.lastIndex);
re.lastIndex = 3.7; m = re.exec(s); out.push(m.index, m[1], re.lastIndex);
re.lastIndex = -4; m = re.exec(s); out.push(m.index, re.lastIndex);
re.lastIndex = 99; out.push(re.exec(s), re.lastIndex);
m = re.exec(s); m = re.exec(s); m = re.exec(s); out.push(m.index, typeof m[1], re.lastIndex, re.test(s), re.lastIndex, re.test(s), re.lastIndex);
out.push(Object.keys(m).join("|"), m.input === s, m.length);
m.index = 7; m.extra = 1; out.push(Object.keys(m).join("|"));
var n = /(\d)(\d)?/.exec("a1b"); out.push(Object.keys(n).join("|"), n.index, n.input, n.length);
var lit = /ab/; out.push(lit.exec("zzab").index, lit.test("a b"), "xxabab".replace(/ab/, "-"), "a,b,,c".split(/,/).length);
out.join(",");
