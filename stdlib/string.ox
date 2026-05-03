// string.ox — bundled bindings for ISO C `<string.h>`.
//
// Imported via `import "string.ox";`. Symbols are resolved by the
// system C library at link time.
//
// Type mapping (C → oxide):
//   int               → i32      (32-bit on every conforming impl)
//   size_t            → usize    (pointer-width primitive; matches size_t ABI)
//   char*  (input)    → *const [u8]
//   char*  (mut buf)  → *mut [u8]
//   void*  (input)    → *const u8
//   void*  (mut buf)  → *mut u8
//
// Verified against (snapshots checked into the repo for offline review):
//   - stdlib/reference/macos/_string.h    (Apple SDK; APSL-2.0 + BSD)
//   - stdlib/reference/musl/string.h      (musl libc; MIT)
// Both libcs report identical signatures for every function in this file.

extern "C" {
    // --- String length & comparison ---
    fn strlen(s: *const [u8]) -> usize;
    fn strcmp(a: *const [u8], b: *const [u8]) -> i32;
    fn strncmp(a: *const [u8], b: *const [u8], n: usize) -> i32;
    // `strcoll` is locale-aware comparison; identical signature to
    // `strcmp`. Result depends on the current `LC_COLLATE` locale.
    fn strcoll(a: *const [u8], b: *const [u8]) -> i32;
    // `strxfrm` writes a transformed form of `src` to `dst` such that
    // memcmp on transformed bytes mirrors strcoll on the originals.
    // Returns the length the transform would have produced (which may
    // exceed `n`; in that case `dst` is unspecified and the caller
    // must allocate more and retry).
    fn strxfrm(dst: *mut [u8], src: *const [u8], n: usize) -> usize;

    // --- String copy & concatenation ---
    fn strcpy(dst: *mut [u8], src: *const [u8]) -> *mut [u8];
    fn strncpy(dst: *mut [u8], src: *const [u8], n: usize) -> *mut [u8];
    fn strcat(dst: *mut [u8], src: *const [u8]) -> *mut [u8];
    fn strncat(dst: *mut [u8], src: *const [u8], n: usize) -> *mut [u8];

    // --- String search ---
    // The search byte is `int` for legacy ISO C reasons; passes via
    // the C ABI's variadic-style i32 promotion. Returns null on miss.
    fn strchr(s: *const [u8], c: i32) -> *const [u8];
    fn strrchr(s: *const [u8], c: i32) -> *const [u8];
    fn strstr(haystack: *const [u8], needle: *const [u8]) -> *const [u8];
    fn strpbrk(s: *const [u8], accept: *const [u8]) -> *const [u8];
    fn strspn(s: *const [u8], accept: *const [u8]) -> usize;
    fn strcspn(s: *const [u8], reject: *const [u8]) -> usize;
    // `strtok` is *not* thread-safe (uses an internal static state);
    // POSIX `strtok_r` is preferred for new code. First call passes
    // `s` non-null; subsequent calls on the same tokenization pass
    // null to advance.
    fn strtok(s: *mut [u8], delim: *const [u8]) -> *mut [u8];

    // --- Memory operations (untyped buffers) ---
    fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8;
    fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8;
    fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8;
    fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32;
    fn memchr(s: *const u8, c: i32, n: usize) -> *const u8;

    // --- Error string lookup ---
    // Returns a static string describing `errnum` (typically the
    // current value of `errno`). Pointer is owned by libc; do not
    // `free` it.
    fn strerror(errnum: i32) -> *const [u8];

    // --- Intentionally NOT bundled (with reason) ---
    //
    // POSIX / BSD / GNU extensions
    //   `strtok_r`, `strerror_r`, `strdup`, `strndup`, `strnlen`,
    //   `stpcpy`, `stpncpy`, `memccpy`, `memmem`, `strsep`, `strlcat`,
    //   `strlcpy`, `strsignal`, `strchrnul`, `strcasestr`, `memrchr`,
    //   `mempcpy` — feature-gated in libc; don't all exist on Windows
    //   MSVC. Will land in a future POSIX bindings file.
    //
    // Locale-parameterized variants (`strerror_l`, `strcoll_l`,
    // `strxfrm_l`)
    //   Take a `locale_t`, an opaque type whose layout differs across
    //   libcs. Defer until oxide has opaque-struct decls.
}
