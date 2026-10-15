// Building a singly linked list and walking it until null, with a field read per node.
// features: loop, object, new, null, call
// expect: 23937000
function ListNode(v, next) { this.v = v; this.next = next; }
function sumList(n) {
  var s = 0;
  while (n !== null) { s += n.v; n = n.next; }
  return s;
}
function main() {
  var head = null;
  for (var i = 0; i < 1000; i++) head = new ListNode(i % 17, head);
  var s = 0;
  for (var r = 0; r < 3000; r++) s += sumList(head);
  return s;
}
main();
