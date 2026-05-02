// Struct literal assigned into an array element — the original
// trigger from example-projects/flappy (`pipes[k] = Pipe { ... }`
// in the restart path). Exercises lvalue=Index{base: Local-of-array}
// + rhs=StructLit with the new aggregate-aware emit_assign.
struct S { a: i32, b: i32 }

fn main() -> i32 {
    let mut arr: [S; 3] = [
        S { a: 1, b: 2 },
        S { a: 3, b: 4 },
        S { a: 5, b: 6 },
    ];
    arr[1] = S { a: 100, b: 200 };
    arr[0].a + arr[1].a + arr[1].b + arr[2].b   // 1 + 100 + 200 + 6 = 307
}
