# Walkthrough

A five-minute tour of Oxide.

## What is Oxide?

**C semantics in Rust syntax.** Oxide is a tiny, ahead-of-time-compiled,
statically-typed language that compiles down to LLVM and links against
native code. If you've written C, the runtime model will feel familiar:
manual memory management, raw pointers, no implicit allocations, no
dispatch overhead, the C ABI for FFI. If you've written Rust, the
surface syntax will too: `let`, `fn`, `mut`, `*const T` / `*mut T`,
`extern "C"`, `as` casts, `if`/`else` as expressions.

What Oxide deliberately does **not** have: closures, generics, traits,
`enum` payloads (the keyword is reserved but not implemented), `match`,
`unsafe`, floats, async, or modules-with-visibility. There is no GC, no
borrow checker, no overload resolution. The type system catches shape
errors and enforces mutability/pointer-aliasing rules at compile time,
then gets out of the way. We ship exactly what's needed to write
idiomatic C through a Rust-shaped lens.

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
`bool`, `char`. No `f32` / `f64` yet. The unit type is `()` and is
written by omitting the return type on a function.

### Strings and pointers

```rust
let s = "hi";              // *const [u8; 3]   — sized byte array pointer
let p: *const i32 = null;  // null pointer literal
let q = &x;                // *const i32       — address of x
let r = &mut y;            // *mut i32         — address of mut y
```

A string literal carries its length in the type (`*const [u8; N]`). When
an `extern "C"` parameter is declared as `*const [u8]`, the length erases
implicitly so you can pass any literal there. Pointer types are
`*const T` and `*mut T`; `*mut T` is assignable to `*const T` but not
the other way around.

### `if` / `else` is an expression

```rust
let max = if a > b { a } else { b };

if x > 0 {
    puts("positive");
} else {
    puts("non-positive");
}
```

Conditions must be `bool`. There is no implicit int-to-bool coercion;
write `x != 0` if you mean it.

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

`for` is C-style (init, condition, step) — not the iterator form.
`break` and `continue` work everywhere.

### Functions and unit return

```rust
fn add(a: i32, b: i32) -> i32 { a + b }

fn shout(s: *const [u8]) {     // returns ()
    puts(s);
}
```

A trailing expression returns; an explicit `return e;` works too.
Bodies that don't produce a value have unit type — omit the `-> ()`.

### Structs

```rust
struct Point { x: i32, y: i32 }

fn origin() -> Point { Point { x: 0, y: 0 } }

let mut p = Point { x: 1, y: 2 };
p.x = 5;        // requires `let mut p`
```

Mutability is per-binding, not per-field. To mutate a single field,
the whole struct binding must be `mut`.

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

`extern "C"` blocks declare functions that link against C code. The
trailing `...` declares C-variadic parameters; you can *call* C
variadics, but you cannot define your own. Narrow integer args at
variadic positions are widened to `i32` automatically (signed-narrow
sign-extends, unsigned-narrow and `bool` zero-extend), matching C's
default argument promotions.

## Building and emitting

`oxide` is a single-file driver: pass the entry point and it walks
imports from there.

| Flag | Effect |
|---|---|
| `--emit exe` (default) | compile, link, run via `execv` |
| `--no-run` | stop after linking; print the binary path to stderr |
| `--emit ir` | print textual LLVM IR to stdout (or `-o` path) |
| `--emit obj` | emit a `.o` object file |
| `--emit lex` / `ast` / `hir` / `typeck` | dump an intermediate representation, useful for tinkering |
| `-O 0\|1\|2\|3\|s\|z` | LLVM optimization level (codegen emits only) |
| `-o <path>` | explicit output path; defaults to `target/oxide-build/<stem>` |

Arguments after `--` are forwarded to the running program:

```sh
oxide hello.ox -- --my-arg
```

## Standard library

Three files are baked into the compiler binary and auto-mount when you
import them by name:

- **`stdio.ox`** — `printf`, `puts`, `getchar`, `fopen` / `fclose`,
  `fread` / `fwrite`, `scanf`, `fflush`, plus the rest of `<stdio.h>`.
- **`string.ox`** — `strlen`, `strcmp`, `strcpy`, `strcat`, `strchr`,
  `strstr`, `memcpy`, `memset`, `memcmp`, plus the rest of `<string.h>`.
- **`stdlib.ox`** — `malloc` / `free` / `realloc`, `exit` / `abort`,
  `getenv`, `system`, `atoi`, `rand` / `srand`.

Use them by importing the bare name:

```rust
import "stdio.ox";
import "string.ox";
import "stdlib.ox";
```

The bundled file wins over a local file of the same name; if you want
to shadow one, name your own file differently. Symbols resolve at link
time against the host's C library (libc on Linux/macOS), so the
available behavior matches what your platform's libc provides.

You can also import your own files by relative path:

```rust
import "./geometry.ox";
```

## Where to next

Browse `example-projects/` in the [repository](https://github.com/hanayashiki/oxide) for end-to-end programs.
Each is a self-contained module you build with the same
`oxide path/to/main.ox` invocation:

- **`puts/`** — the hello-world above.
- **`fib/`** — recursive Fibonacci with a C-extern `print_int` callout.
- **`socket-server/`** — a small HTTP server demonstrating structs,
  `&mut`, and `loop`.
- **`flappy/`** — a TUI game using arrays (`[u8; N]`), mutable indexing,
  and nested loops.

Pick whichever looks fun, copy it out, and start changing things.
