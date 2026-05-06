import "stdio.ox";
import "mem.ox";
import "./imported.ox";

fn cmp(a: *const i32, b: *const i32) -> i32 {
    *a - *b
}

fn main() {
    let mut to_sort = [3, 2, 1];
    let length = 3;
    sort(&mut to_sort, length, cmp);

    for (let mut i = 0; i < length; i += 1) {
        printf("%d\n", to_sort[i]);
    }
}

fn sort<T>(arr: *mut [T], n: usize, cmp: fn(a: *const T, b: *const T) -> i32) {
    let mut sorted = false;

    while !sorted {
        sorted = true;

        for (let mut i = 0; i < n - 1; i += 1) {
            let cmp_result = cmp(&arr[i], &arr[i+1]);
            if cmp_result > 0 {
                let tmp = arr[i + 1];
                arr[i + 1] = arr[i];
                arr[i] = tmp;
                sorted = false;
            }
        }
    }
}
