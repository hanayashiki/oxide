// io.ox — byte I/O on raw fds.
//
// Stage 0 lacks `extern "C" { static stderr; }`, so a portable
// stdio-based stderr handle is unavailable. We side-step by
// writing directly to fds via `write(2)`. The OS handles
// buffering at exit.
//
// Buffered output: callers accumulate into a `StrBuf` and flush
// via `io_print_strbuf` once. v0 issues each write as a single
// `write(2)` call without retrying on short writes — fine for
// regular files; partial-write handling can land later if pipes
// become a problem.

import "string.ox";        // strlen
import "../util/strbuf.ox";

extern "C" {
    fn write(fd: i32, buf: *const [u8], n: usize) -> isize;
}

fn io_stdout_fd() -> i32 { 1 }
fn io_stderr_fd() -> i32 { 2 }

fn io_print(buf: *const [u8], len: usize) -> isize {
    write(io_stdout_fd(), buf, len)
}

fn io_eprint(buf: *const [u8], len: usize) -> isize {
    write(io_stderr_fd(), buf, len)
}

fn io_print_strbuf(s: *const StrBuf) -> isize {
    write(io_stdout_fd(), strbuf_as_ptr(s), strbuf_len(s))
}

fn io_eprint_strbuf(s: *const StrBuf) -> isize {
    write(io_stderr_fd(), strbuf_as_ptr(s), strbuf_len(s))
}

fn io_print_cstr(s: *const [u8]) -> isize {
    write(io_stdout_fd(), s, strlen(s))
}

fn io_eprint_cstr(s: *const [u8]) -> isize {
    write(io_stderr_fd(), s, strlen(s))
}
