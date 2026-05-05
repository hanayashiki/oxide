import "mem.ox";

fn main() -> i32 {
    let i = 0;
    let p = &i;
    if ox_is_null(p) && !ox_is_not_null(p) {
        1
    } else {
        2
    }
}
