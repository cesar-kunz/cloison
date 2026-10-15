// Recursively allocated binary trees, walked recursively (the Benchmarks Game's binary-trees).
// features: recursion, object, new, null, call
// expect: 327640
function TreeNode(left, right) { this.left = left; this.right = right; }
function bottomUp(depth) {
  return depth > 0 ? new TreeNode(bottomUp(depth - 1), bottomUp(depth - 1)) : new TreeNode(null, null);
}
function checkTree(t) { return t.left === null ? 1 : 1 + checkTree(t.left) + checkTree(t.right); }
function main() {
  var s = 0;
  for (var r = 0; r < 40; r++) s += checkTree(bottomUp(12));
  return s;
}
main();
