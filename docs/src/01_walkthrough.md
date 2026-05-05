# Walkthrough

A five-minute tour of Oxide.

## What is Oxide?

**C semantics in Rust syntax.** Oxide is a tiny, ahead-of-time-compiled,
statically-typed language that lowers to LLVM and links against native
code. If you've written C, the runtime model will feel familiar: manual
memory management, raw pointers, no implicit allocations, no dispatch
overhead, the C ABI for FFI. If you've written Rust, the surface syntax
will too — `let`, `fn`, `fn name<T>`, `mut`, `*const T` / `*mut T`,
`extern "C"`, `as` casts, `if`/`else` as expressions.

What Oxide deliberately does **not** have: closures, traits, `enum`
payloads (the keyword is reserved but unimplemented), `match`, `unsafe`,
floats, async, or modules-with-visibility. No GC, no borrow checker, no
overload resolution. The type system catches shape errors and enforces
mutability and pointer-aliasing rules at compile time, then gets out of
the way. The goal is idiomatic C with Rust-shaped syntax — nothing more.

## Install

The fastest way to get the `oxide` binary onto your `$PATH`:

```sh
curl -sSf https://oxide.cwang.io/install.sh | sh
```

## A first program

```rust
import "stdio.ox";

fn main() -> i32 {
    puts("hello world");
    0
}
```

Build and run:

```sh
oxide hello.ox
```

## Tour by example

### Bindings

```rust
let x = 1;            // immutable
let mut y = 0;        // mutable
y = y + 1;
let n: i32 = 42;      // type annotation optional
```

Integer literals default to `i32`. Widen or narrow with `as`.

### Primitives

`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, `isize`, `usize`,
`bool`. No `f32` / `f64` yet. The unit type `()` has no surface
spelling — write it by omitting a function's return type.

> Use `u8` instead of `char`.

### Strings and pointers

```rust
let s = "hi";              // *const [u8; 3]   — sized byte array pointer
let p: *const i32 = null;  // null pointer literal
let q = &x;                // *const i32       — address of x
let r = &mut y;            // *mut i32         — address of mut y
```

A string literal carries its length in its type (`*const [u8; N]`). An
`extern "C"` parameter declared as `*const [u8]` erases the length so
any literal fits. Pointer types come in two flavors, `*const T` and
`*mut T`; `*mut T` coerces to `*const T`, but not the other way around.

### `if` / `else` is an expression

```rust
let max = if a > b { a } else { b };

if x > 0 {
    puts("positive");
} else {
    puts("non-positive");
}
```

Conditions must be `bool` — no implicit int-to-bool coercion. Write
`x != 0` if you mean it.

### Loops

```rust
while i < n { i = i + 1; }

for (let mut i = 0; i < 4; i = i + 1) {
    // body
}

loop {
    if done { break; }
}
```

`for` is C-style (init, condition, step), not the iterator form.
`break` and `continue` work everywhere.

### Functions and unit return

```rust
fn add(a: i32, b: i32) -> i32 { a + b }

fn shout(s: *const [u8]) {     // returns ()
    puts(s);
}
```

A trailing expression is the return value; explicit `return e;` works
too. A body that produces no value has unit type — drop the `-> ()`.

### Structs

```rust
struct Point { x: i32, y: i32 }

fn origin() -> Point { Point { x: 0, y: 0 } }

let mut p = Point { x: 1, y: 2 };
p.x = 5;        // requires `let mut p`
```

Mutability is per-binding, not per-field: to mutate a single field,
the whole struct binding must be `mut`.

### Pointer usage

As in C, a pointer dereference is valid on either side of an assignment:

```rust
let mut n = 1;
let ptr_to_n = &mut n;           // *mut i32

*ptr_to_n = 42;                  // writes through the pointer; n is now 42
let m = *ptr_to_n;               // reads through the pointer; m is 42
```

`&` yields `*const T` (read-only); `&mut` yields `*mut T` (write-through).

Field access on a struct pointer auto-dereferences, so the explicit
`(*ptr).field` form is unnecessary (and there is no `->` operator):

```rust
let mut p = Point { x: 1, y: 2 };
let ptr_to_p = &mut p;

ptr_to_p.x = 5;                  // auto-deref
(*ptr_to_p).x = 5;               // explicit form, equivalent
```

### `extern "C"` and variadics

```rust
extern "C" {
    fn printf(fmt: *const [u8], ...) -> i32;
}

fn main() -> i32 {
    let n: u8 = 42;
    printf("n = %d\n", n);   // u8 zero-extends to i32 at the call site
    0
}
```

An `extern "C"` block declares functions that link against C symbols.
A trailing `...` marks a C-variadic parameter list — you can _call_ C
variadics, but you can't define your own. Narrow integer args at
variadic positions widen to `i32` automatically (signed-narrow types
sign-extend, unsigned-narrow and `bool` zero-extend), matching C's
default argument promotions.

## Generic types

Both `fn`s and `struct`s can take type parameters.

Generic structs:

```rust
struct LinkedList<T> {
    value: T,
    next: *mut LinkedList<T>,
}

// ✅ explicitly typed
let mut linked_list = LinkedList::<i32> {
    value: 0,
    next: null,
};

// ✅ inferred
let mut linked_list = LinkedList {
    value: 0,
    next: null,
};
```

Generic functions:

```rust
fn id<T>(x: T) {
    x
}

id::<i32>(x);   // ✅ explicitly typed
id(1);          // ✅ inferred, same as above

id::<[i32]>(x); // ❌ `T` must have a known size
```

Inside a generic body, a value of generic type can only be copied —
assignments and pass-throughs are fine; operations that would require
knowing more about `T` are not.

```rust
// ✅ can be assigned around
fn swap<T>(a: *mut T, b: *mut T) {
    let c = *a;
    *a = *b;
    *b = c;
}

// ✅ can be passed as a parameter
fn eat<T>(x: T) {
    eat(x);
}

// ❌ rejected: `T` may not support comparison
fn compare<T>(a: T, b: T) -> bool {
    a < b
}
```

## Building and emitting

`oxide` is a single-file driver: pass the entry point, and it walks
imports from there.

| Flag                                    | Effect                                                        |
| --------------------------------------- | ------------------------------------------------------------- |
| `--emit exe` (default)                  | compile, link, run via `execv`                                |
| `--no-run`                              | stop after linking; print the binary path to stderr           |
| `--emit ir`                             | print textual LLVM IR to stdout (or `-o` path)                |
| `--emit obj`                            | emit a `.o` object file                                       |
| `--emit lex` / `ast` / `hir` / `typeck` | dump an intermediate representation, useful for tinkering     |
| `-O 0\|1\|2\|3\|s\|z`                   | LLVM optimization level (codegen emits only)                  |
| `-o <path>`                             | explicit output path; defaults to `target/oxide-build/<stem>` |

Arguments after `--` are forwarded to the running program:

```sh
oxide hello.ox -- --my-arg
```

## Memory management

`mem.ox` ships typed wrappers around the C allocator: `ox_alloc<T>`,
`ox_alloc_zeroed<T>`, `ox_dealloc<T>`, `ox_ptr_eq<T>`, and `ox_realloc<T>`. Import it
like any other stdlib file.

```rust
import "stdio.ox";
import "mem.ox";

struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let integer = ox_alloc::<i32>();           // uninitialized
    *integer = 42;

    let point = ox_alloc_zeroed::<Point>();    // zero-initialized
    printf("point.x = %d\n", point.x);         // → "point.x = 0"

    ox_dealloc(integer);
    ox_dealloc(point);
    0
}
```

## C standard library

Three header-shaped bindings are bundled with the compiler and import
under their bare names:

- **`stdio.ox`** — `printf`, `puts`, `getchar`, `fopen` / `fclose`,
  `fread` / `fwrite`, `scanf`, `fflush`, plus the rest of `<stdio.h>`.
- **`string.ox`** — `strlen`, `strcmp`, `strcpy`, `strcat`, `strchr`,
  `strstr`, `memcpy`, `memset`, `memcmp`, plus the rest of `<string.h>`.
- **`stdlib.ox`** — `malloc` / `free` / `realloc`, `exit` / `abort`,
  `getenv`, `system`, `atoi`, `rand` / `srand`.

```rust
import "stdio.ox";
import "string.ox";
import "stdlib.ox";

fn main() -> i32 {
    printf("Hello, %d\n", 42);
    0
}
```

A bundled file wins over a local file with the same name — rename your
own to disambiguate. Symbols resolve at link time against the host's C
library (libc on Linux and macOS), so the available behavior tracks
your platform's libc.

Your own files import by relative path:

```rust
import "./geometry.ox";
```

## Where to next

Browse `example-projects/` in the [repository](https://github.com/hanayashiki/oxide)
for end-to-end programs. Each is a self-contained module you build
with the same `oxide path/to/main.ox` invocation:

- **`puts/`** — the hello-world above.
- **`fib/`** — recursive Fibonacci with a C-extern `print_int` callout.
- **`layout_intrinsics/`** — a tour of `ox_size_of`, `ox_transmute`,
  and the `mem.ox` typed allocator wrappers.
- **`layout_mem/`** — a minimal `ox_alloc` / `ox_dealloc` round-trip.
- **`socket-server/`** — a small HTTP server built on structs, `&mut`,
  and `loop`.
- **`flappy/`** — a TUI game using arrays (`[u8; N]`), mutable
  indexing, and nested loops.

Pick whichever looks fun, copy it out, and start changing things.
