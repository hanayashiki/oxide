// `*p.x` must parse as `*(p.x)` — postfix Field (level 12) binds
// tighter than prefix `*` (level 13). To say "deref then field",
// write `(*p).x` (see deref_paren_field.ox).
fn f(p: Holder) {
    *p.q;
}
