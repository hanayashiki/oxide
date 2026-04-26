struct Inner { z: i32 }
struct Outer { inner: Inner }

fn f(o: Outer) {
    o.inner.z;
}
