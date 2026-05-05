// End-to-end smoke for the user's motivating example: a `slurp`
// helper that opens a file, checks for null via ox_ptr_eq, reads
// some bytes, and closes. Exercises the integer-comparison
// obligation (cap + (1 as usize)), pointer-equality via
// ox_ptr_eq (replacing the old f == null), and the as-cast
// validation (1 as usize is IntToInt). See spec/05/07/12.
import "mem.ox";
import "stdio.ox";

fn slurp(path: *const [u8], buf: *mut u8, cap: usize) -> usize {
    let mode: *const [u8] = "r";
    let f = fopen(path, mode);
    if ox_ptr_eq(f, null) {
        return cap + (1 as usize);
    }
    let n = fread(buf, 1 as usize, cap, f);
    fclose(f);
    n
}
