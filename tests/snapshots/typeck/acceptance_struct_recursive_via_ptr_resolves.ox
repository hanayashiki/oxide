struct Node { next: *const Node }

fn pass(n: *const Node) -> *const Node {
    n
}
