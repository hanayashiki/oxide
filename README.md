# Oxide

An educational compiler that writes like Rust and compiles like C, targeting LLVM.

⚠️ Currently just for recreation, and still a work in progress

The documentation is hosted at [oxide.cwang.io](https://oxide.cwang.io/).

# Why Oxide?

- Performance: compiled to LLVM then to machine code, getting down to raw metal as its name suggests
- Modern: Rust developers should feel at home with its Rust-like syntax and generic types.
- Fun: No unsafe blocks, no annoying checks — juggle pointers with sheer joy.

# Examples

## Hello World

Pull in libc's stdio and print your first line of Oxide.

```rust
// hello.ox
import "stdio.ox";

fn main() -> i32 {
    puts("hello world");
    0
}
```

```bash
> oxide hello.ox
hello world
```

## Linked List

Monomorphized to concrete machine code at every call site.

```rust
// linked-list.ox
import "stdio.ox";
import "mem.ox";

struct Node<T> {
    value: T,
    next: *mut Node<T>,
}

// Push a new node onto the front of the list. Returns the new head.
fn push<T>(head: *mut Node<T>, v: T) -> *mut Node<T> {
    let n = ox_alloc::<Node<T>>();
    n.value = v;
    n.next = head;
    n
}

fn len<T>(head: *mut Node<T>) -> i32 {
    let mut count: i32 = 0;
    let mut cur = head;
    while !ox_ptr_eq(cur, null) {
        count = count + 1;
        cur = cur.next;
    }
    count
}

fn free_all<T>(head: *mut Node<T>) {
    let mut cur = head;
    while !ox_ptr_eq(cur, null) {
        let next = cur.next;
        ox_dealloc(cur);
        cur = next;
    }
}

fn main() -> i32 {
    let mut head: *mut Node<i32> = null;
    head = push(head, 10);
    head = push(head, 20);
    head = push(head, 30);

    printf("length = %d\n", len(head));

    let mut cur = head;
    while !ox_ptr_eq(cur, null) {
        printf("%d -> ", cur.value);
        cur = cur.next;
    }
    puts("(nil)");

    free_all(head);
    0
}

```

```rust
> oxide linked-list.ox
length = 3
30 -> 20 -> 10 -> (nil)
```

# Example Projects

Go to [./example-projects](./example-projects/) for end-to-end programs.

# License

This project is publicly available and hopefully you can learn some compiler knowledge from it.

[MIT](README.md/LICENSE)
