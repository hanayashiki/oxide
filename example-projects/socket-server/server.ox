// A minimal "Hello, world!" HTTP server in pure Oxide.
//
// Demonstrates: extern "C" FFI to libc / Darwin sockets, struct
// construction, field assignment, address-of (&/&mut), and an
// infinite `loop { }` for the accept loop.
//
// Tested on macOS (Darwin) — the sockaddr_in layout below matches
// macOS's BSD-style struct (sin_len + 1-byte sin_family). Linux uses
// 2-byte sin_family with no sin_len; this won't run as-is on Linux.
//
// To run:   ./run.sh
// To test:  curl -i http://localhost:8080/

// macOS sockaddr_in layout (16 bytes):
//   offset 0:  sin_len    : u8   (=16)
//   offset 1:  sin_family : u8   (=2 for AF_INET)
//   offset 2:  sin_port   : u16  (network byte order)
//   offset 4:  sin_addr   : u32  (network byte order; 0 = INADDR_ANY)
//   offset 8:  sin_zero   : 8 bytes padding (modeled as u64)
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

// Accept connections forever, writing the canned response to each.
fn serve(server_fd: i32) -> i32 {
    let mut addr = sockaddr_in {
        sin_len: 16,
        sin_family: 2,
        sin_port: 0,
        sin_addr: 0,
        sin_zero: 0,
    };
    let mut addr_len: u32 = 16;

    // Hardcoded response. We deliberately don't read() the request —
    // toy server, doesn't dispatch on path / method. The kernel buffers
    // the request bytes; they get discarded on close.
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 13\r\n\r\nHello, world!";

    loop {
        // accept() writes the actual address length back into addr_len,
        // so reset to the buffer size before each call.
        addr_len = 16;
        let client_fd = accept(server_fd, &mut addr, &mut addr_len);
        if client_fd < 0 {
            perror("accept");
            return 1;
        }

        write(client_fd, response, 78);
        close(client_fd);
    }
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
