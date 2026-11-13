// An exit at a setter's receiver check resumes at the store, with the
// value in `acc` (the check reads it: `Insn::store_check`).
// expect: 0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,52,53,54,55,56,57,58,59

function Id() { this.f = 0; }
Object.defineProperty(Id.prototype, "key", { set: function (v) { this._key = v; } });
function main() {
  var a = new Id(), b = new Id(); b.extra = 1;
  var c = new Id(); c.other = 2; c.more = 3;
  var objs = [a, b], out = [];
  for (var r = 0; r < 60; r++) {
    var o = objs[r & 1];
    if (r == 50) o = c;
    o.key = r;
    out.push(o._key);
  }
  return out.join(",");
}
main();
