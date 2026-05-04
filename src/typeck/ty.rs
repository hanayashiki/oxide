//! Type vocabulary: hash-cons interner (`TyArena`) and the `TyKind` enum.
//!
//! Equal types share a `TyId` because every construction goes through
//! `TyArena::intern`. Codegen and typeck can compare types via
//! `id == id` instead of walking structures.
//!
//! `TyArena` uses interior mutability: `intern(&self, ...)` so callers
//! (typeck inferer, mono cascade, codegen lazy substitution) don't have
//! to thread `&mut TyArena` through their call chains. Storage is
//! `RefCell<IndexVec<TyId, &'static TyKind>>` — the elements are
//! leaked at insertion time, giving `kind(&self, id) -> &TyKind` a
//! stable address with a lifetime tied to `&self` (no `Ref` guard
//! scoping problems for recursive-walk callers like `substitute_ty`).
//!
//! The leak per fresh kind is bounded by the program's unique-type
//! count (interner deduplicates; cache hits don't leak); for a v0
//! compile, ~few hundred KB of leaked TyKind objects, reclaimed by
//! the OS at process exit. The forward path (when long-running
//! embeddings like LSP/REPL become real) is to parameterize TyArena
//! with an explicit `'arena` lifetime backed by `bumpalo::Bump` —
//! the API surface (`intern(&self, ...)` / `kind(&self, ...) -> &TyKind`)
//! is forward-compatible.

use std::cell::RefCell;
use std::collections::HashMap;

use index_vec::IndexVec;

use crate::hir::{AdtKind, FieldIdx, VariantIdx};
use crate::parser::ast::Mutability;
use crate::reporter::Span;

index_vec::define_index_type! { pub struct TyId    = u32; }
index_vec::define_index_type! { pub struct InferId = u32; }
// Typeck-side ADT identity. Distinct from HIR's `HAdtId`; today the
// numbering is 1:1 (allocated in `decl::resolve_decls` phase 0), but
// the indirection leaves room for future generic-instantiation
// many-to-one without renaming every `Adt(_)` site.
index_vec::define_index_type! { pub struct AdtId   = u32; }
// Typeck-side type-parameter identity. Distinct from HIR's `HTyParamId`
// for the same reason as AdtId — Param is a type-system entity (not
// syntactic), so it follows the AdtId/HAdtId precedent: separate
// newtypes on either side of the HIR/typeck boundary, related 1:1 via
// `ParamId::from_raw(htyparam.raw())`. See spec/16_GENERIC.md §HIR.
index_vec::define_index_type! { pub struct ParamId = u32; }

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum TyKind {
    Prim(PrimTy),
    /// `()` — zero-sized.
    Unit,
    /// `!` — bottom type. Subtype of every type during unification.
    Never,
    /// Function signature. Third tuple element is the C-ABI variadic
    /// flag (`true` only for `extern "C" fn(..., ...)`); see
    /// spec/15_VARIADIC.md.
    Fn(Vec<TyId>, TyId, bool),
    /// `*const T` / `*mut T`. Mutability is interned alongside the pointee
    /// so the arena distinguishes the two variants. Unify treats them
    /// equivalently (shape only); the coercion check at use sites enforces
    /// the actual `mut → const` direction rule. See `spec/07_POINTER.md`.
    Ptr(TyId, Mutability),
    /// Identity-only handle to an ADT. Structural data (fields, variants)
    /// lives in `TypeckResults.adts[aid]`; equality is `aid == aid`.
    /// See `spec/08_ADT.md` "Typeck phase ordering and ADT vocabulary".
    Adt(AdtId),
    /// `[T; N]` (sized — `Some(n)`) or `[T]` (unsized — `None`). The
    /// unified shape mirrors the `[T] ≡ [T; ∞]` mental model directly.
    /// `Array(_, None)` is rejected as a value type at typeck (E0269);
    /// HIR carries the shape through unchanged so typeck can see
    /// through type aliases (future). Length is stored inline as `u64`
    /// — earlier draft used a `ConstArena` interner but that layer was
    /// dropped (zero v0 benefit; const generics will reintroduce a
    /// richer length representation when that spec lands). See
    /// spec/09_ARRAY.md.
    Array(TyId, Option<u64>),
    /// Unification variable; resolved via the per-fn `Inferer`.
    Infer(InferId),
    /// Reference to a generic type parameter declared by the enclosing
    /// fn. A leaf at typeck — only mono substitutes Param leaves into
    /// concrete types. Permitted to appear in `expr_tys`/`local_tys`
    /// for generic-fn bodies; see spec/16_GENERIC.md §Typeck rules.
    Param(ParamId),
    /// Poison; absorbs without further errors.
    Error,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PrimTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Bool,
    /// Unsigned target-pointer-sized integer. v0 is target-fixed at
    /// 64-bit, so codegen lowers `Usize` to LLVM `i64` (same as `U64`).
    /// The type system carries the semantic distinction from day one —
    /// `usize` and `u64` are NOT interconvertible without an explicit
    /// `as` cast. See spec/09_ARRAY.md "New primitives".
    Usize,
    /// Signed target-pointer-sized integer. Same v0 lowering and
    /// distinctness rules as `Usize`.
    Isize,
}

impl PrimTy {
    pub fn name(&self) -> &'static str {
        match self {
            PrimTy::I8 => "i8",
            PrimTy::I16 => "i16",
            PrimTy::I32 => "i32",
            PrimTy::I64 => "i64",
            PrimTy::U8 => "u8",
            PrimTy::U16 => "u16",
            PrimTy::U32 => "u32",
            PrimTy::U64 => "u64",
            PrimTy::Bool => "bool",
            PrimTy::Usize => "usize",
            PrimTy::Isize => "isize",
        }
    }

    pub fn is_integer(&self) -> bool {
        !matches!(self, PrimTy::Bool)
    }
}

#[derive(Clone, Debug)]
pub struct FnSig {
    pub params: Vec<TyId>,
    pub ret: TyId,
    /// Type parameters in declaration order. Empty for non-generic fns.
    /// Each `ParamId` is 1:1 with HIR's `HTyParamId` via
    /// `ParamId::from_raw(htypid.raw())`. See spec/16_GENERIC.md.
    pub generic_params: Vec<ParamId>,
    /// `true` while the placeholder sig is in `Checker::new`, before
    /// `decl::resolve_decls` phase 1 fills in real param/ret TyIds.
    /// Flipped to `false` once resolved. Reading a partial FnSig from
    /// outside the build phases is a typeck bug.
    ///
    /// Note: today this flag is mostly ceremonial — phase 1 is single-
    /// pass and nothing reads `fn_sig` between `Checker::new` and the
    /// flip, so there's no observable partial-FnSig state in the
    /// current pipeline. Kept for symmetry with `AdtDef::partial` and
    /// in case fn signatures ever grow a real multi-pass shape (generics,
    /// trait method default impls, where-clause resolution).
    pub partial: bool,
    /// C-ABI variadic flag. Mirrored on `TyKind::Fn`'s third tuple
    /// element. Named `c_variadic` (not `is_variadic`) per
    /// spec/15_VARIADIC.md to disambiguate from possible future
    /// Rust-style variadic generics; the simpler `is_variadic` name
    /// only lives at the HIR layer.
    pub c_variadic: bool,
}

/// Typed ADT definition. Built up across phases 0 and 0.5 in
/// `decl::resolve_decls`: phase 0 pushes a stub with empty `variants`
/// and `partial: true`; phase 0.5 backfills the variants/fields with
/// resolved `TyId`s and flips `partial` to `false`. Indexed in
/// `Checker.adts` / `TypeckResults.adts` by `AdtId`.
#[derive(Clone, Debug)]
pub struct AdtDef {
    pub name: String,
    pub kind: AdtKind,
    pub variants: IndexVec<VariantIdx, VariantDef>,
    pub partial: bool,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: Option<String>,
    pub fields: IndexVec<FieldIdx, FieldDef>,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TyId,
    pub span: Span,
}

/// Hash-cons interner with interior-mutable storage.
///
/// Not `Clone` — `RefCell` doesn't deep-clone, and a clone-snapshot of
/// the type universe at a point in time isn't meaningful (the leaked
/// TyKinds keep their `'static` references regardless). Wrap in
/// `Rc`/`Arc` if shared-ownership ever becomes a need; v0 has exactly
/// one TyArena per compilation, owned by `TypeckResults`.
#[derive(Debug)]
pub struct TyArena {
    /// Append-only storage of leaked TyKinds. Indexed by `TyId`. The
    /// element type is `&'static TyKind` because each push goes through
    /// `Box::leak` — this gives stable addresses without coupling the
    /// returned `&TyKind` to any `Ref<'_, ...>` guard scope.
    arena: RefCell<IndexVec<TyId, &'static TyKind>>,
    /// Hash-cons map: dedups interned TyKinds. Mutation is short-lived
    /// (one `borrow_mut` per fresh insert) so re-entrant `intern` calls
    /// from inside the same arena would runtime-panic — but the only
    /// callers either dedupe immediately or recurse through TyArena
    /// methods that release the borrow before re-entering.
    interner: RefCell<HashMap<TyKind, TyId>>,
    pub i8: TyId,
    pub i16: TyId,
    pub i32: TyId,
    pub i64: TyId,
    pub u8: TyId,
    pub u16: TyId,
    pub u32: TyId,
    pub u64: TyId,
    pub bool: TyId,
    pub usize: TyId,
    pub isize: TyId,
    pub unit: TyId,
    pub never: TyId,
    pub error: TyId,
}

impl Default for TyArena {
    fn default() -> Self {
        Self::new()
    }
}

impl TyArena {
    pub fn new() -> Self {
        // Build an empty arena, then prime the primitive shortcuts via
        // the interior-mut `intern`. The `&self` API works the moment
        // the struct is constructed — we just need temporary `TyId`s
        // for the field initializers, which we obtain by populating
        // primitives first.
        let arena = Self {
            arena: RefCell::new(IndexVec::new()),
            interner: RefCell::new(HashMap::new()),
            // Sentinels overwritten immediately after construction.
            // `from_raw(0)` is a placeholder — none of these reads
            // observe the sentinel because the Self return below
            // uses the interned ids from the local bindings.
            i8: TyId::from_raw(0),
            i16: TyId::from_raw(0),
            i32: TyId::from_raw(0),
            i64: TyId::from_raw(0),
            u8: TyId::from_raw(0),
            u16: TyId::from_raw(0),
            u32: TyId::from_raw(0),
            u64: TyId::from_raw(0),
            bool: TyId::from_raw(0),
            usize: TyId::from_raw(0),
            isize: TyId::from_raw(0),
            unit: TyId::from_raw(0),
            never: TyId::from_raw(0),
            error: TyId::from_raw(0),
        };
        let i8 = arena.intern(TyKind::Prim(PrimTy::I8));
        let i16 = arena.intern(TyKind::Prim(PrimTy::I16));
        let i32 = arena.intern(TyKind::Prim(PrimTy::I32));
        let i64 = arena.intern(TyKind::Prim(PrimTy::I64));
        let u8 = arena.intern(TyKind::Prim(PrimTy::U8));
        let u16 = arena.intern(TyKind::Prim(PrimTy::U16));
        let u32 = arena.intern(TyKind::Prim(PrimTy::U32));
        let u64 = arena.intern(TyKind::Prim(PrimTy::U64));
        let bool = arena.intern(TyKind::Prim(PrimTy::Bool));
        let usize = arena.intern(TyKind::Prim(PrimTy::Usize));
        let isize = arena.intern(TyKind::Prim(PrimTy::Isize));
        let unit = arena.intern(TyKind::Unit);
        let never = arena.intern(TyKind::Never);
        let error = arena.intern(TyKind::Error);
        Self {
            arena: arena.arena,
            interner: arena.interner,
            i8,
            i16,
            i32,
            i64,
            u8,
            u16,
            u32,
            u64,
            bool,
            usize,
            isize,
            unit,
            never,
            error,
        }
    }

    /// Intern a `TyKind`, returning its `TyId`. Cache-deduped by the
    /// hash-cons interner so equal kinds share an id. Fresh kinds are
    /// `Box::leak`ed to give the underlying `&TyKind` a stable address
    /// (Vec growth on the spine doesn't move boxed contents); the leak
    /// is bounded by the program's unique-type count.
    pub fn intern(&self, kind: TyKind) -> TyId {
        // Fast path: cache hit — no allocation, no leak, no spine push.
        if let Some(&id) = self.interner.borrow().get(&kind) {
            return id;
        }
        // Slow path: not interned yet. Leak a Box<TyKind> for stable
        // address, then push the &'static reference into the arena and
        // record the kind→id mapping in the interner.
        let leaked: &'static TyKind = Box::leak(Box::new(kind.clone()));
        let id = self.arena.borrow_mut().push(leaked);
        self.interner.borrow_mut().insert(kind, id);
        id
    }

    /// Look up the `TyKind` for a given `TyId`. Returns a `&TyKind` whose
    /// lifetime is tied to `&self`; the underlying memory is leaked
    /// `'static` so the caller can hold the reference across subsequent
    /// `intern()` calls (which is what `substitute_ty`'s recursive match
    /// arms need — `tys.kind(ty)` borrowed across `tys.intern(...)`).
    pub fn kind(&self, id: TyId) -> &TyKind {
        // The element is `&'static TyKind` (Copy reference). Pulling it
        // out from under the `Ref` guard with `*` copies the static ref
        // into a temporary that outlives the guard.
        *self
            .arena
            .borrow()
            .get(id)
            .expect("TyId out of range for TyArena")
    }

    /// Look up a primitive type by its source-level name. `None` if the
    /// name is not a primitive (i.e., it's a user-defined type).
    pub fn from_prim_name(&self, name: &str) -> Option<TyId> {
        Some(match name {
            "i8" => self.i8,
            "i16" => self.i16,
            "i32" => self.i32,
            "i64" => self.i64,
            "u8" => self.u8,
            "u16" => self.u16,
            "u32" => self.u32,
            "u64" => self.u64,
            "bool" => self.bool,
            "usize" => self.usize,
            "isize" => self.isize,
            "void" => self.unit,
            "never" => self.never,
            _ => return None,
        })
    }

    /// Render a type for diagnostics. Resolved types only — caller should
    /// pass through the inferer's `resolve` first if there might be infer
    /// vars.
    pub fn render(&self, id: TyId) -> String {
        match self.kind(id) {
            TyKind::Prim(p) => p.name().to_string(),
            TyKind::Unit => "()".to_string(),
            TyKind::Never => "!".to_string(),
            TyKind::Fn(params, ret, c_variadic) => {
                let mut s = String::from("fn(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&self.render(*p));
                }
                if *c_variadic {
                    if !params.is_empty() {
                        s.push_str(", ");
                    }
                    s.push_str("...");
                }
                s.push(')');
                if *ret != self.unit {
                    s.push_str(" -> ");
                    s.push_str(&self.render(*ret));
                }
                s
            }
            TyKind::Ptr(inner, m) => format!("*{} {}", m.as_str(), self.render(*inner)),
            // Bare arena rendering doesn't have access to the AdtDef table —
            // print just the identity. A future `TypeckResults`-aware
            // Printer can resolve the name. See spec/08_ADT.md "Render".
            TyKind::Adt(aid) => format!("Adt({})", aid.raw()),
            TyKind::Array(elem, None) => format!("[{}]", self.render(*elem)),
            TyKind::Array(elem, Some(n)) => format!("[{}; {}]", self.render(*elem), n),
            TyKind::Infer(id) => format!("?T{}", id.raw()),
            // Param renders as `Param(<raw>)` to match the existing
            // `Adt(<raw>)` style. Source-name rendering (e.g. `T`)
            // requires HIR context (`hir.ty_params[...].name`) and is
            // deferred to a UX-polish pass — see spec/16_GENERIC.md
            // §Typeck rules. Snapshot tests assert on the raw form.
            TyKind::Param(pid) => format!("Param({})", pid.raw()),
            TyKind::Error => "{error}".to_string(),
        }
    }
}
