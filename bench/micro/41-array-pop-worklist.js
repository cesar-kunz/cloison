// LIFO worklist: push seeds, pop one, push its successors, until empty.
// features: loop, array, new
// expect: 409400
function main() {
  var visited = 0;
  for (var r = 0; r < 200; r++) {
    var todo = new Array();
    todo.push(1);
    while (todo.length > 0) {
      var n = todo.pop();
      visited++;
      if (n < 1024) { todo.push(2 * n); todo.push(2 * n + 1); }
    }
  }
  return visited;
}
main();
