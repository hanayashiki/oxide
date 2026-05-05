// IntLit annotated with bool — the literal types as integer
// (decl-phase rule pins via annotation), but bool isn't an integer
// Prim, so discharge_subtype's Prim-Prim arm fires E0250.
const X: bool = 0;
