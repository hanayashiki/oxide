// A TUI Flappy Bird in pure Oxide.
//
// Demonstrates: structs, fixed-size arrays, mutable indexing/field
// chains, `&`/`&mut` for FFI buffers, `loop { }` + `for (;;)` with
// `break`/`continue`, `if`-as-expression, block-as-expression, modulo,
// `as` casts, and `extern "C"` to libc.
//
// Type-annotation policy. Most locals are inferred — int literals
// default to `i32`, bool literals to `bool`, struct/array literals
// from their RHS shape, and counters bind to `usize` automatically
// when they're used as an array index. We only annotate where the
// inferred type would be wrong:
//
//   - FFI buffers (`[u8; N]`) — literal ints default to i32, so the
//     element type has to be pinned.
//   - `t: u64` — `time()` writes `time_t` (u64-shaped) through the ptr.
//   - `i: i64` — `read()` returns `i64`; `i < n` would otherwise
//     unify n (i64) and i (default i32) and fail.
//
// Controls: SPACE = flap, q = quit, r = restart after game over.
// Tested on macOS/Linux terminals (uses ANSI cursor escape codes).
//
// To run:  ./run.sh
//
// Notes on terminal setup. We use `system("stty ...")` to put the
// terminal into a non-canonical, no-echo mode where `read` returns
// immediately whether or not a key is pending (`min 0 time 0`). This
// avoids needing a `termios` struct binding (its layout differs
// across platforms) or `fcntl`/`poll` plumbing for a small example.
//
// Tunables: canvas is 80 cols x 24 rows, pipe gap is 8 rows tall,
// frame rate is ~12 fps (80 ms/frame), pipes scroll every other tick.

extern "C" {
    fn write(fd: i32, buf: *const [u8], count: u64) -> i64;
    fn read(fd: i32, buf: *mut [u8], count: u64) -> i64;
    fn usleep(usec: u32) -> i32;
    fn system(cmd: *const [u8]) -> i32;
    fn rand() -> i32;
    fn srand(seed: u32);
    fn time(t: *mut u64) -> u64;
}

// One pipe = a vertical column with an 8-row gap. `x` is the screen
// column; pipes scroll left and recycle past the left edge.
struct Pipe {
    x: i32,
    gap_top: i32,
}

fn main() -> i32 {
    // ANSI sequences as raw byte arrays — sidesteps any string-escape
    // questions in the lexer. ESC=27, [=91, ?=63, H=72, J=74, l=108, h=104.
    //                             ESC  [   ?   2   5   l  ESC  [   2   J
    let init_seq:    [u8; 10] = [27, 91, 63, 50, 53, 108, 27, 91, 50, 74];
    //                             ESC  [   ?   2   5   h  ESC  [   2   J  ESC [ H
    let restore_seq: [u8; 13] = [27, 91, 63, 50, 53, 104, 27, 91, 50, 74, 27, 91, 72];

    // Put terminal into non-canonical, no-echo, non-blocking-read mode.
    system("stty -icanon -echo min 0 time 0");
    write(1, &init_seq, 10);

    // Seed RNG from current time. `time(NULL)` would need a pointer-
    // int cast, which is deferred (spec/12_AS.md), so we pass the
    // address of a local instead. The `t` annotation is load-bearing:
    // `time` wants `*mut u64`, so the local has to be u64.
    let mut t: u64 = 0;
    time(&mut t);
    srand(t as u32);

    // Game state. Bird starts centered (row 12 of 24). All inferred
    // — int literals default to i32, `true` to bool, struct literals
    // from their declared field types.
    let mut bird_y  = 12;
    let mut bird_vy = 0;
    let mut tick    = 0;
    let mut score   = 0;
    let mut alive   = true;

    // Four pipes, recycled left-to-right. Initial spacing 25 cols
    // off-screen-right; canvas width is 80, so the first pipe enters
    // after ~10 ticks of scrolling. Type inferred as `[Pipe; 4]`
    // from the array literal.
    let mut pipes = [
        Pipe { x: 90,  gap_top: 8  },
        Pipe { x: 115, gap_top: 12 },
        Pipe { x: 140, gap_top: 4  },
        Pipe { x: 165, gap_top: 14 },
    ];

    // Render buffer big enough for: cursor-home + 24*(80+1) + score +
    // slack. `[u8; N]` annotations needed because integer literals
    // alone would default to `i32`.
    let mut buf:   [u8; 4096] = [0; 4096];
    let mut input: [u8; 8]    = [0; 8];

    loop {
        // ---- input ----------------------------------------------------
        let n = read(0, &mut input, 8);
        if n > 0 {
            // `i: i64` so the `i < n` compare unifies cleanly with
            // `read`'s `i64` return. Default i32 would mismatch.
            let mut i: i64 = 0;
            while i < n {
                // `c` inferred as u8 from `input`'s element type.
                let c = input[i as usize];
                if c == 113 {              // 'q' — quit
                    write(1, &restore_seq, 13);
                    system("stty icanon echo");
                    return 0;
                }
                if c == 32 && alive {      // SPACE — flap
                    bird_vy = -2;
                }
                if c == 114 && !alive {    // 'r' — restart after death
                    bird_y  = 12;
                    bird_vy = 0;
                    tick    = 0;
                    score   = 0;
                    alive   = true;
                    pipes[0] = Pipe { x: 90,  gap_top: 8  };
                    pipes[1] = Pipe { x: 115, gap_top: 12 };
                    pipes[2] = Pipe { x: 140, gap_top: 4  };
                    pipes[3] = Pipe { x: 165, gap_top: 14 };
                }
                i = i + 1;
            }
        }

        // ---- physics & world tick ------------------------------------
        if alive {
            if tick % 2 == 0 {
                bird_vy = bird_vy + 1;
                if bird_vy > 2 { bird_vy = 2; }

                // `k` is unannotated; the `pipes[k]` index inside the
                // body forces it to bind to `usize`, which then
                // propagates to the cond and update slots.
                for (let mut k = 0; k < 4; k = k + 1) {
                    pipes[k].x = pipes[k].x - 1;
                    if pipes[k].x == 9 { score = score + 1; }
                    if pipes[k].x < -1 {
                        pipes[k].x = 85;
                        // Block-as-expression: the `{ … }` runs the
                        // inner let + if-expression and yields an
                        // `i32` for the surrounding `+ 2`.
                        pipes[k].gap_top = {
                            let r = rand() % 13;       // [-12, 12]
                            if r < 0 { -r } else { r } // → [0, 12]
                        } + 2;                          // → [2, 14]
                    }
                }
            }
            bird_y = bird_y + bird_vy;

            // ---- collision -------------------------------------------
            if bird_y < 0 || bird_y >= 24 { alive = false; }
            for (let mut k = 0; k < 4; k = k + 1) {
                if pipes[k].x == 10
                    && (bird_y < pipes[k].gap_top || bird_y >= pipes[k].gap_top + 8) {
                    alive = false;
                }
            }
        }

        // ---- render --------------------------------------------------
        // Cursor home (no clear — we overwrite in place to avoid flicker).
        // `off` binds to `usize` automatically via `buf[off]` indexing.
        let mut off = 0;
        buf[off] = 27;  off = off + 1;     // ESC
        buf[off] = 91;  off = off + 1;     // [
        buf[off] = 72;  off = off + 1;     // H

        for (let mut row = 0; row < 24; row = row + 1) {
            for (let mut col = 0; col < 80; col = col + 1) {
                // `if` as expression for the initial cell value. `ch`
                // binds to `u8` via the eventual `buf[off] = ch` store.
                let mut ch = if col == 10 && row == bird_y {
                    62                     // '>' — the bird
                } else {
                    32                     // ' '
                };

                for (let mut k = 0; k < 4; k = k + 1) {
                    if pipes[k].x == col
                        && (row < pipes[k].gap_top || row >= pipes[k].gap_top + 8) {
                        ch = 35;           // '#' — pipe wall
                    }
                }

                buf[off] = ch;
                off = off + 1;
            }
            buf[off] = 10;                 // '\n'
            off = off + 1;
        }

        // "score: NNN"
        buf[off] = 115; off = off + 1;     // s
        buf[off] = 99;  off = off + 1;     // c
        buf[off] = 111; off = off + 1;     // o
        buf[off] = 114; off = off + 1;     // r
        buf[off] = 101; off = off + 1;     // e
        buf[off] = 58;  off = off + 1;     // :
        buf[off] = 32;  off = off + 1;     // space

        if score == 0 {
            buf[off] = 48; off = off + 1;  // '0'
        } else {
            // Itoa: scratch out digits low-to-high, then copy reversed.
            // `count` and `di` infer to `usize` via array indexing.
            let mut digits: [u8; 5] = [0; 5];
            let mut count = 0;
            let mut x = score;
            while x > 0 {
                digits[count] = ((x % 10) + 48) as u8;
                x = x / 10;
                count = count + 1;
            }
            for (let mut di = 0; di < count; di = di + 1) {
                buf[off + di] = digits[count - 1 - di];
            }
            off = off + count;
        }

        if !alive {
            // "  GAME OVER (r=restart)"
            let msg: [u8; 23] = [
                32, 32, 71, 65, 77, 69, 32, 79, 86, 69, 82, 32,
                40, 114, 61, 114, 101, 115, 116, 97, 114, 116, 41,
            ];
            for (let mut mi = 0; mi < 23; mi = mi + 1) {
                buf[off] = msg[mi];
                off = off + 1;
            }
        }

        // Pad with spaces so a previously-longer status line is overwritten.
        for (let mut pi = 0; pi < 30; pi = pi + 1) {
            buf[off] = 32;
            off = off + 1;
        }
        buf[off] = 10; off = off + 1;      // trailing newline

        write(1, &buf, off as u64);

        usleep(80000);                     // ~12 fps
        tick = tick + 1;
    }
    // Note: this function returns `i32`, but the tail expression is
    // `loop { ... }`. The loop has no `break`, so it types as `!`
    // (Never), which absorbs into any context — including the i32
    // return slot. The only normal-exit path is the `return 0` inside
    // the `q`-quit branch.
}
