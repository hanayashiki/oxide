struct Point { x: i32, y: i32 }

fn make() -> Point { make() }

fn f() {
    make().x;
}
