// `Uint8Array`'s base64 and hex: both alphabets, padding, the last
// chunk's handling, and the `setFrom` methods writing what they decoded
// before an error.
// expect: +/8AaGk= -_8AaGk fbff006869 | 251,255,0,104,105 | 251,255,0,104,105 | SyntaxError | SyntaxError | 0 | 0,255,171 | SyntaxError | 8,4,104,105,106,107 | SyntaxError 1,2,0 | TypeError

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
var b = new Uint8Array([251, 255, 0, 104, 105]);
t(() => [b.toBase64(), b.toBase64({ alphabet: "base64url", omitPadding: true }), b.toHex()].join(" "));
t(() => Uint8Array.fromBase64("+/8AaGk=").join());
t(() => Uint8Array.fromBase64(" -_8A aGk", { alphabet: "base64url" }).join());
t(() => Uint8Array.fromBase64("aGk", { lastChunkHandling: "strict" }));
t(() => Uint8Array.fromBase64("aGl=", { lastChunkHandling: "strict" }));
t(() => Uint8Array.fromBase64("aGk", { lastChunkHandling: "stop-before-partial" }).length);
t(() => Uint8Array.fromHex("00ffAb").join());
t(() => Uint8Array.fromHex("0g"));
t(() => { var into = new Uint8Array(4); var x = into.setFromBase64("aGlqaw=="); return [x.read, x.written, into.join()].join(); });
t(() => { var into = new Uint8Array(3); try { into.setFromHex("0102zz"); } catch (e) { return e.name + " " + into.join(); } });
t(() => Uint8Array.prototype.toHex.call(new Int8Array(1)));
r.join(" | ");
