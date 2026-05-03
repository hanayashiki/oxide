// stdio.ox — bundled bindings for ISO C `<stdio.h>`.
//
// Imported via `import "stdio.ox";`. Symbols are resolved by the
// system C library at link time (cc supplies libc by default).
//
// Type mapping (C → oxide):
//   int               → i32      (32-bit on every conforming impl)
//   size_t            → usize    (pointer-width primitive; matches size_t ABI)
//   ssize_t           → isize    (signed pointer-width)
//   long, unsigned long → avoid (LLP64 vs LP64 width split)
//   long long         → i64      (always 64-bit per C standard)
//   char*  (input)    → *const [u8]   (matches example-projects/puts/hello.ox)
//   char*  (mut buf)  → *mut [u8]
//   void*             → *mut u8       (or *const u8 per ABI)
//   FILE*, opaque     → *mut u8       (oxide has no opaque-struct decl yet)
//   void return       → omit `->`     (e.g. `fn perror(s: *const [u8]);`)
//
// Variadic functions (printf, fprintf, scanf, …) declare with `...`
// per spec/15_VARIADIC.md. Variadic args promote per C ABI: integer
// args narrower than i32 sign- or zero-extend to i32; floats narrower
// than f64 widen to f64 (when floats land).
//
// Verified against (snapshots checked into the repo for offline review):
//   - stdlib/reference/macos/_stdio.h     (Apple SDK; APSL-2.0 + BSD)
//   - stdlib/reference/musl/stdio.h       (musl libc; MIT)
// Both libcs report identical signatures for every function in this file.

extern "C" {
    // --- Character & string output ---
    fn puts(s: *const [u8]) -> i32;
    fn putchar(c: i32) -> i32;
    fn putc(c: i32, stream: *mut u8) -> i32;            // = fputc; macro on some libcs
    fn fputs(s: *const [u8], stream: *mut u8) -> i32;
    fn fputc(c: i32, stream: *mut u8) -> i32;

    // --- Character input ---
    fn getchar() -> i32;
    fn getc(stream: *mut u8) -> i32;                    // = fgetc; macro on some libcs
    fn fgetc(stream: *mut u8) -> i32;
    fn ungetc(c: i32, stream: *mut u8) -> i32;
    // `fgets` reads up to `size - 1` bytes (plus a NUL terminator) into
    // `buf` from `stream`. Returns `buf` on success, null on EOF/error.
    // The size arg is `int` per C standard, hence `i32` (not `usize`).
    fn fgets(buf: *mut [u8], size: i32, stream: *mut u8) -> *mut [u8];

    // --- Stream open/close ---
    fn fopen(path: *const [u8], mode: *const [u8]) -> *mut u8;
    fn freopen(path: *const [u8], mode: *const [u8], stream: *mut u8) -> *mut u8;
    fn fclose(stream: *mut u8) -> i32;

    // --- Temporary streams ---
    // `tmpfile` opens a unique temporary file in binary update mode
    // ("wb+") and returns a `FILE*`; the file is auto-removed when
    // closed or on normal exit. Same signature on macOS and musl.
    //
    // `tmpnam` is intentionally NOT bundled. Reasons:
    //   - The required buffer size (`L_tmpnam`) differs across libcs
    //     (macOS = 1024, musl = 20); without const items in oxide
    //     there is no portable way to size the caller's buffer.
    //   - macOS marks `tmpnam` deprecated due to TOCTOU races and
    //     recommends `mkstemp(3)` instead.
    // Use `tmpfile` (or, once POSIX bindings land, `mkstemp`).
    fn tmpfile() -> *mut u8;

    // --- Filesystem operations on paths ---
    fn remove(path: *const [u8]) -> i32;
    fn rename(old_path: *const [u8], new_path: *const [u8]) -> i32;

    // --- Block I/O ---
    fn fread(buf: *mut u8, size: usize, count: usize, stream: *mut u8) -> usize;
    fn fwrite(buf: *const u8, size: usize, count: usize, stream: *mut u8) -> usize;

    // --- Stream state & flushing ---
    fn fflush(stream: *mut u8) -> i32;
    fn feof(stream: *mut u8) -> i32;
    fn ferror(stream: *mut u8) -> i32;
    fn clearerr(stream: *mut u8);
    fn rewind(stream: *mut u8);

    // --- Buffering control ---
    // `mode` is one of `_IOFBF` (full), `_IOLBF` (line), `_IONBF` (none).
    // These macros aren't exposed by oxide yet; users pass the literal
    // values 0 / 1 / 2 respectively. `setbuf(stream, NULL)` disables
    // buffering; otherwise `buf` must be at least `BUFSIZ` bytes (1024
    // on macOS, 1024 on musl glibc — the constant is the same in
    // practice, but treat as opaque until oxide has const items).
    fn setvbuf(stream: *mut u8, buf: *mut [u8], mode: i32, size: usize) -> i32;
    fn setbuf(stream: *mut u8, buf: *mut [u8]);

    // --- Diagnostics ---
    fn perror(s: *const [u8]);

    // --- Variadic formatted I/O (per spec/15_VARIADIC.md) ---
    fn printf(fmt: *const [u8], ...) -> i32;
    fn fprintf(stream: *mut u8, fmt: *const [u8], ...) -> i32;
    fn sprintf(buf: *mut [u8], fmt: *const [u8], ...) -> i32;
    fn snprintf(buf: *mut [u8], size: usize, fmt: *const [u8], ...) -> i32;
    fn scanf(fmt: *const [u8], ...) -> i32;
    fn fscanf(stream: *mut u8, fmt: *const [u8], ...) -> i32;
    fn sscanf(s: *const [u8], fmt: *const [u8], ...) -> i32;

    // --- Intentionally NOT bundled (with reason) ---
    //
    // `fseek(FILE*, long, int) -> int` and `ftell(FILE*) -> long`
    //   The offset parameter / return type is C `long`, which is
    //   64-bit on Linux/macOS LP64 but 32-bit on Windows MSVC LLP64.
    //   No portable oxide mapping. Wait for type aliases or per-triple
    //   variants. POSIX `fseeko`/`ftello` use `off_t` (always 64-bit
    //   on POSIX) and will land in a future POSIX bindings file.
    //
    // `fgetpos(FILE*, fpos_t*) -> int` and `fsetpos(FILE*, const fpos_t*) -> int`
    //   `fpos_t` is an opaque type whose layout differs across libcs;
    //   without an opaque-struct declaration in oxide there is no
    //   portable way to allocate one. Skip until oxide has opaque
    //   types or until callers are willing to use `*mut u8` of the
    //   right hand-tracked size.
    //
    // `gets(char*) -> char*`
    //   Removed from C11 due to unbounded buffer overflow. macOS
    //   marks it deprecated; musl still declares it for legacy. Use
    //   `fgets(buf, size, stdin)` instead.
    //
    // `v*printf` / `v*scanf` family — see the va_list note below.
    //
    // Stream constants `stdin` / `stdout` / `stderr`
    //   These are `extern FILE *` globals (or macros referencing them
    //   via `__stdinp`/`__stdoutp`/`__stderrp` on macOS, an array on
    //   musl). Oxide doesn't yet support `extern "C" { static X: T; }`;
    //   add when extern globals land.
    //
    // POSIX / GNU / BSD extensions
    //   `fdopen`, `popen`, `pclose`, `fileno`, `fseeko`, `ftello`,
    //   `dprintf`, `getline`, `getdelim`, `fmemopen`, `open_memstream`,
    //   `setlinebuf`, `flockfile`, `getc_unlocked`, … are not in ISO C
    //   `<stdio.h>` and don't exist on Windows MSVC. Will land in a
    //   future `unistd.ox` (POSIX) file once a target-family check
    //   exists.

    // --- v*printf / v*scanf family — intentionally NOT bundled ---
    // The `v*` variants take a `va_list` (already-collected variadic
    // args) as their last parameter. Per spec/15_VARIADIC.md:96, v0
    // variadic is call-site only — there is no `va_list` type, no
    // `va_start`/`va_arg`/`va_end`, and no defining-side `...`. So
    // there is no way for oxide code to construct an argument to pass
    // these functions; the declarations would be dead externs.
    //
    // Light up with `va_list` once the spec adds it:
    //   fn vprintf(fmt: *const [u8], ap: va_list) -> i32;
    //   fn vfprintf(stream: *mut u8, fmt: *const [u8], ap: va_list) -> i32;
    //   fn vsprintf(buf: *mut [u8], fmt: *const [u8], ap: va_list) -> i32;
    //   fn vsnprintf(buf: *mut [u8], size: usize, fmt: *const [u8], ap: va_list) -> i32;
}
