// A field type generalized by a store in the interpreter drops the code
// of callers bound to a version that took the old type, not only that
// version's (`Vm::rely_through`). Mandreel's `malloc`, reduced: its
// version read global `tlsf_ptr` as an int32; the interpreter stored a
// double there after an exit, and the loop's direct call kept entering
// that version, which read the double's low half (0) and set up the heap
// again. The expected value is Node's.
// expect: 160000,4096,321000,160000,4096,321000,160000,4096,321000,160000,4096,321000

var heap = new ArrayBuffer(1 << 16), heap32 = new Int32Array(heap), heapU32 = new Uint32Array(heap);
var r_g0 = 0, tlsf_ptr = 0, next = 0, bad = "";
function tlsf_create(sp) { next = 1000; r_g0 = 4096; }
function tlsf_malloc(sp) { var p = next; next = (next + heap32[(sp + 4) >> 2] + 7) & ~7; r_g0 = p; }
function setD() { r_g0 = 11856.5; }
function malloc(sp) {
  var size = heapU32[sp >> 2];
  if (size == 0) { size = 4; }
  if (tlsf_ptr == 0) {
    var addr = 77;
    sp -= 8;
    heap32[(sp) >> 2] = addr;
    heap32[(sp + 4) >> 2] = 1234;
    tlsf_create(sp);
    tlsf_ptr = r_g0;
  }
  sp -= 8;
  heap32[(sp) >> 2] = tlsf_ptr;
  heap32[(sp + 4) >> 2] = size;
  tlsf_malloc(sp);
  if (r_g0 == 0) { bad = bad || "malloc failed"; }
  return;
}
function user(sp, n) {
  var s = 0;
  for (var i = 0; i < n; i++) { heap32[sp >> 2] = 16; malloc(sp); var r5 = r_g0; if (r5 != 0) { s = (s + (r5 & 15)) | 0; } }
  return s;
}
var res = [];
for (var it = 0; it < 4; it++) {
  tlsf_ptr = 0; setD();
  res.push(user(512, 20000), tlsf_ptr, next);
}
bad || res.join();
