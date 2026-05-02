# Oxide

An educational compiler that writes like Rust and compiles like C, targeting LLVM.

⚠️ Currently just for recreation, and still a work in progress

# Examples

## Hello World

```rust
extern "C" {
    fn puts(s: *const u8) -> i32;
}

fn main() -> i32 {
    puts("hello world");
    puts("goodbye");
    0
}
```

```bash
hello world
goodbye
```

## Socket Server

```rust
// A minimal "Hello, world!" HTTP server in pure Oxide.
//
// Demonstrates: extern "C" FFI to libc / Darwin sockets, struct
// construction, field assignment, address-of (&/&mut), recursion as
// a substitute for the missing `while` loop.
struct sockaddr_in {
    sin_len: u8,
    sin_family: u8,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: u64,
}

extern "C" {
    fn socket(domain: i32, ty: i32, protocol: i32) -> i32;
    fn bind(fd: i32, addr: *const sockaddr_in, len: u32) -> i32;
    fn listen(fd: i32, backlog: i32) -> i32;
    fn accept(fd: i32, addr: *mut sockaddr_in, len: *mut u32) -> i32;
    fn write(fd: i32, buf: *const u8, count: u64) -> i64;
    fn close(fd: i32) -> i32;
    fn htons(host: u16) -> u16;
    fn perror(s: *const u8);
    fn puts(s: *const u8) -> i32;
}

// Accept one connection, write the canned response, close, recurse.
// No `while`-loop yet — recursion is the v0 substitute.
fn serve(server_fd: i32) -> i32 {
    let mut addr = sockaddr_in {
        sin_len: 16,
        sin_family: 2,
        sin_port: 0,
        sin_addr: 0,
        sin_zero: 0,
    };
    let mut addr_len: u32 = 16;

    let client_fd = accept(server_fd, &mut addr, &mut addr_len);
    if client_fd < 0 {
        perror("accept");
        return 1;
    }

    // Hardcoded response. We deliberately don't read() the request —
    // toy server, doesn't dispatch on path / method. The kernel buffers
    // the request bytes; they get discarded on close.
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!";
    write(client_fd, response, 78);
    close(client_fd);

    // Tail-recurse to keep accepting. Without TCO this leaks stack
    // per request — fine for the demo, would need real `while` for
    // production.
    serve(server_fd)
}

fn main() -> i32 {
    let mut addr = sockaddr_in {
        sin_len: 16,
        sin_family: 2,        // AF_INET
        sin_port: 0,          // set below via htons
        sin_addr: 0,          // INADDR_ANY
        sin_zero: 0,
    };
    addr.sin_port = htons(8080);

    let server_fd = socket(2, 1, 0);     // AF_INET, SOCK_STREAM, default proto
    if server_fd < 0 {
        perror("socket");
        return 1;
    }

    if bind(server_fd, &addr, 16) < 0 {
        perror("bind");
        return 1;
    }

    if listen(server_fd, 10) < 0 {
        perror("listen");
        return 1;
    }

    puts("Listening on http://localhost:8080/");
    serve(server_fd)
}
```

Server output:

```bash
Listening on http://localhost:8080/
```

Client output:

```bash
curl -i http://localhost:8080/ 
HTTP/1.1 200 OK
Content-Type: text/plain
Content-Length: 13

Hello, world!
```

# References

- [Mapping High Level Constructs to LLVM IR](https://mapping-high-level-constructs-to-llvm-ir.readthedocs.io/en/latest/)

# License

This project is publicly available and hopefully you can learn some compiler knowledge from it.

[MIT](README.md/LICENSE)
