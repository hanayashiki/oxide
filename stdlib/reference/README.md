# stdlib/reference

Verbatim C standard-library headers used as the verification source
for the bundled oxide bindings in `stdlib/*.ox`. These are reference
material only — they are **not** included in the compiler build, not
compiled, and not loaded at runtime.

When editing a binding in `stdlib/<lib>.ox`, cross-check the
signature against the matching prototype here on **both** platforms
(macOS Apple-fork + musl). If the two diverge for a function, the
binding is unsafe to ship cross-platform — drop it or split into
per-triple variants.

## Layout

- `macos/` — copied verbatim from a local Xcode SDK
  (`/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk/usr/include/`).
  Files: `_stdio.h`, `_stdlib.h`, `_string.h`, `_abort.h`, `_malloc.h`.
  License: APSL-2.0 + BSD (preserved in the original file headers).
- `musl/` — fetched from
  `https://git.musl-libc.org/cgit/musl/plain/include/{stdio,stdlib,string}.h`.
  Files: `stdio.h`, `stdlib.h`, `string.h`.
  License: MIT (preserved at the top of each file or in the project's
  COPYRIGHT file).

## Refreshing

These are static snapshots taken at the time the bundled bindings
were verified. Refresh only when bumping bindings (rare). Commands:

```bash
# macOS
SDK=$(xcrun --show-sdk-path)
cp "$SDK/usr/include/_stdio.h"          stdlib/reference/macos/
cp "$SDK/usr/include/_stdlib.h"         stdlib/reference/macos/
cp "$SDK/usr/include/_string.h"         stdlib/reference/macos/
cp "$SDK/usr/include/_abort.h"          stdlib/reference/macos/
cp "$SDK/usr/include/malloc/_malloc.h"  stdlib/reference/macos/_malloc.h

# musl
for h in stdio stdlib string; do
  curl -sSL "https://git.musl-libc.org/cgit/musl/plain/include/$h.h" \
    -o "stdlib/reference/musl/$h.h"
done
```
