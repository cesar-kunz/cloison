// Slices made through `trim`, a String object's methods and slices of
// slices are read by the operations that copy units a word at a time
// past their end (concatenation, `split`, `JSON`, spreading), and `.*`'s
// skip after a failed attempt starts the next one at a code point: in `u`
// mode, past a line terminator, a surrogate pair is one unit of `.`/`[^a]`
// (ECMA-262 22.2.2: the input is a list of code points). Expected values
// from V8, SpiderMonkey, JavaScriptCore and QuickJS.
// expect: 2990,9000,9002,9002,5504,113,500,500,1/1097,7800,true,true,true,true,6,%uD83D%uDE00b,1,%uD83D%uDE00b,1,%uD83D%uDE00b

var out = [];
var big = 'x'.repeat(5000) + 'abc' + 'y'.repeat(5000);
out.push(String.prototype.substring.call(new String(big), 10, 3000).length);
var t = '  ' + 'z'.repeat(9000) + '  '; t.charCodeAt(0);
out.push(t.trim().length, t.trimStart().length, t.trimEnd().length);
var u = big.slice(4000), parts = [];
for (var i = 0; i < 50; i++) { u = u.slice(10); parts.push(u + 'q'); }
out.push(parts[49].length, parts[49].charCodeAt(parts[49].length - 1), u.indexOf('abc'), u.lastIndexOf('abc'));
out.push(big.slice(4999, 6100).split('abc').map(function (x) { return x.length; }).join('/'));
var sl = big.slice(100, 4000), c = sl + sl;
out.push(c.length, c.slice(3800, 5000) === 'x'.repeat(1200), JSON.parse(JSON.stringify(big.slice(2000, 4000))) === 'x'.repeat(2000));
out.push([...big.slice(4000, 6000)].join('') === big.slice(4000, 6000), Symbol.for(big.slice(2, 1100)) === Symbol.for('x'.repeat(1098)));
var s = '1\ud83d%x\n 😀b\ude00\n ';
var m = /.*[^a]b/mu.exec(s); out.push(m.index, escape(m[0]));
m = /.*[^a]b/u.exec(' 😀b'); out.push(m.index, escape(m[0]));
m = /.*[^a]b/.exec(' 😀b'); out.push(m.index, escape(m[0]));
out.join()
