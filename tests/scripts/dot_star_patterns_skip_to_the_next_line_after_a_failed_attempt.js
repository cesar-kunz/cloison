// A pattern beginning with `.*` resumes its search past the next line
// terminator after a failed attempt (`Regex::dot_star`), as no match can
// start before it. The flags (`m`, `s`, `u`, `i`, `y`, `g`), `lastIndex`,
// lookbehind and lookahead, captures and backreferences, `replace` and
// `split` find what they did. Expected values from V8 and SpiderMonkey.
// expect-json: "10:[\"zz%2a%3fq\"]:0 ;; 10:[\"zz%2a%3fq\"]:0 ;; 23:[\"c%7e\"]:0 ;; 23:[\"c%7e\"]:0 ;; 0:[\"aaa\\nab%41\\nzz%2a%3fq\\r\\nb\u2028c%7e\"]:0 ;; 21:[\"b\"]:0 ;; 0:[\"x\"]:0 ;; 10:[\"zz%2a%3fq\"]:0 ;; 10:[\"zz%\",\"z\"]:0 ;; 4:[\"ab%\"]:0 ;; 4:[\"ab%41\",\"%41\"]:0 ;; 4419|103f18|237e27 ;; 4:[\"ab\"]:6 ;; null:0 ;; 12:[\"%2a\"]:15 ;; null:0 ;; 3:[\"😀\"]:0 ;; 3:[\"😀x\"]:0 ;; 0:[\"a😀\"]:0 ;; \"aaa\\n<41>\\n<3f>q\\r\\nb\u2028<7e>\" ;; 2 ;; [\"c\"] ;; null:0 ;; 2:[\"aa\\nb\"]:0 ;; 2:[\"aa\"]:0"

var out = [];
function ex(re, s) { re.lastIndex = re.lastIndex; var m = re.exec(s); return m ? m.index + ':' + JSON.stringify(Array.from(m)) + ':' + re.lastIndex : 'null:' + re.lastIndex; }
var s1 = 'aaa\nab%41\nzz%2a%3fq\r\nb\u2028c%7e';
out.push(ex(/.*(?:%[\da-f]{2}.*){2}/i, s1));
out.push(ex(/.*(?:%[\da-f]{2}.*){2}/, s1));
out.push(ex(/.*%7e/, s1), ex(/.*c%7e/, s1), ex(/.*%7e/s, s1));
out.push(ex(/.*^b/m, s1), ex(/.*$/m, 'x\ny'), ex(/.*q$/m, s1));
out.push(ex(/.*(z)\1%/, s1), ex(/.*(?<=b)%/, s1), ex(/.*?(%\w\w)/, s1));
var g = /.*%(\w\w)/g; var all = []; var m; while ((m = g.exec(s1))) all.push(m.index + m[1] + g.lastIndex); out.push(all.join('|'));
var y = /.*b/y; y.lastIndex = 4; out.push(ex(y, s1)); y.lastIndex = 1; out.push(ex(y, s1));
var gl = /.*%2a/g; gl.lastIndex = 12; out.push(ex(gl, s1)); gl.lastIndex = 20; out.push(ex(gl, s1));
out.push(ex(/.*\u{1F600}/u, 'ab\n\u{1F600}x'), ex(/.*x/u, '\u{1F600}\n\u{1F600}x'), ex(/.*?\uDE00/, 'a\uD83D\uDE00'));
out.push(JSON.stringify(s1.replace(/.*%(\w\w)/g, '<$1>')), s1.split(/.*%3f/).length, JSON.stringify('a\nb\nc'.match(/.*c/g)));
out.push(ex(/.*(?:\n)x/, 'aa\nbx'), ex(/.*\nb/, 'q\naa\nb'), ex(/.*(?=\nb)/, 'q\naa\nb'));
out.join(' ;; ')
