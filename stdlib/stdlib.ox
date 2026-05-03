// stdlib.ox — bundled bindings for ISO C `<stdlib.h>`.
//
// Imported via `import "stdlib.ox";`. Symbols are resolved by the
// system C library at link time.
//
// Type mapping (C → oxide):
//   int               → i32      (32-bit on every conforming impl)
//   size_t            → usize    (pointer-width primitive; matches size_t ABI)
//   long, unsigned long → avoid (LLP64 vs LP64 width split)
//   long long         → i64      (always 64-bit per C standard)
//   char*  (input)    → *const [u8]
//   void*             → *mut u8       (or *const u8 per ABI)
//   void return       → omit `->`
//
// Verified against (snapshots checked into the repo for offline review):
//   - stdlib/reference/macos/_stdlib.h    (Apple SDK; APSL-2.0 + BSD)
//   - stdlib/reference/macos/_malloc.h    (where malloc/free actually live)
//   - stdlib/reference/macos/_abort.h
//   - stdlib/reference/musl/stdlib.h      (musl libc; MIT)
// Both libcs report identical signatures for every function in this file.

extern "C" {
    // --- Memory allocation ---
    fn malloc(size: usize) -> *mut u8;
    fn calloc(count: usize, size: usize) -> *mut u8;
    fn realloc(ptr: *mut u8, size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
    // C11. Allocates `size` bytes aligned to `alignment` (which must be
    // a power of two and a multiple of `sizeof(void*)`). `size` must
    // be a multiple of `alignment`. Returns null on failure.
    fn aligned_alloc(alignment: usize, size: usize) -> *mut u8;

    // --- Process termination ---
    // `exit` runs atexit handlers + flushes stdio; `_Exit` skips both
    // and calls the kernel directly (used in forked children before
    // exec). `quick_exit` (C11) runs at_quick_exit handlers but skips
    // atexit. `abort` raises SIGABRT.
    //
    // All four are `_Noreturn` in C; oxide doesn't track that yet, so
    // they declare as void-returning and the caller is responsible for
    // any control-flow assumptions past the call.
    fn exit(status: i32);
    fn _Exit(status: i32);
    fn quick_exit(status: i32);
    fn abort();

    // --- Numeric parsing (fixed-width returns only) ---
    fn atoi(s: *const [u8]) -> i32;
    fn atoll(s: *const [u8]) -> i64;

    // --- Integer absolute value ---
    fn abs(x: i32) -> i32;
    fn llabs(x: i64) -> i64;

    // --- Random numbers (PRNG; not cryptographically secure) ---
    fn rand() -> i32;                       // returns [0, RAND_MAX]
    fn srand(seed: u32);                    // C signature is `unsigned`

    // --- Environment ---
    // Returns `*const char` in C ("must not modify"); we use
    // `*const [u8]`. Returns null on miss — callers should null-check.
    fn getenv(name: *const [u8]) -> *const [u8];

    // --- Shell command ---
    // Passes `cmd` to `/bin/sh -c` (or the platform shell). Returns
    // the shell's exit status, or -1 on failure to spawn. `system(NULL)`
    // returns nonzero iff a shell is available (per ISO C).
    fn system(cmd: *const [u8]) -> i32;

    // --- Intentionally NOT bundled (with reason) ---
    //
    // Long-returning numeric parsers
    //   `atol`, `strtol`, `strtoul` — return `long`, which is i64 on
    //   Linux/macOS LP64 but i32 on Windows MSVC LLP64. No portable
    //   oxide mapping. Use `atoi` / `atoll` (fixed widths). For full
    //   `strtoll` / `strtoull` (with the `char **endptr` outparam),
    //   wait until oxide accepts double-pointer types `*mut *const T`
    //   in extern decls.
    //
    // Floating-point conversions
    //   `atof`, `strtof`, `strtod`, `strtold` — oxide has no `f32` /
    //   `f64` primitives yet. Light up alongside floats.
    //
    // Function-pointer-taking APIs
    //   `atexit`, `at_quick_exit`, `bsearch`, `qsort` — require a
    //   function-pointer type in oxide. Defer.
    //
    // Struct-returning APIs
    //   `div`, `ldiv`, `lldiv` — return `div_t` / `ldiv_t` / `lldiv_t`
    //   structs by value. Oxide can't yet declare these as `extern "C"`
    //   return types (struct-by-value ABI work pending).
    //
    // Wide-character / multi-byte conversions
    //   `mblen`, `mbtowc`, `wctomb`, `mbstowcs`, `wcstombs` — require
    //   a `wchar_t` type, whose width is platform-specific (16-bit on
    //   Windows, 32-bit on Unix). Defer until oxide has type aliases
    //   or a `cfg`-attribute system.
    //
    // POSIX / BSD / GNU extensions
    //   `posix_memalign`, `setenv`, `unsetenv`, `mkstemp`, `mkdtemp`,
    //   `realpath`, `random`, `drand48`, `putenv`, `clearenv`, … all
    //   live behind feature-test macros in libc and don't exist on
    //   Windows MSVC. Will land in a future POSIX bindings file.
}
