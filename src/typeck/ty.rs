//! Type vocabulary: hash-cons interner (`TyArena`) and the `TyKind` enum.
//!
//! Equal types share a `TyId` because every construction goes through
//! `TyArena::intern`. Codegen and typeck can compare types via
//! `id == id` instead of walking structures.

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

#[derive(Clone, Debug)]
pub struct TyArena {
    arena: IndexVec<TyId, TyKind>,
    interner: HashMap<TyKind, TyId>,
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
        let mut arena = IndexVec::<TyId, TyKind>::new();
        let mut interner = HashMap::new();
        let mut intern = |kind: TyKind| -> TyId {
            if let Some(&id) = interner.get(&kind) {
                return id;
            }
            let id = arena.push(kind.clone());
            interner.insert(kind, id);
            id
        };
        let i8 = intern(TyKind::Prim(PrimTy::I8));
        let i16 = intern(TyKind::Prim(PrimTy::I16));
        let i32 = intern(TyKind::Prim(PrimTy::I32));
        let i64 = intern(TyKind::Prim(PrimTy::I64));
        let u8 = intern(TyKind::Prim(PrimTy::U8));
        let u16 = intern(TyKind::Prim(PrimTy::U16));
        let u32 = intern(TyKind::Prim(PrimTy::U32));
        let u64 = intern(TyKind::Prim(PrimTy::U64));
        let bool = intern(TyKind::Prim(PrimTy::Bool));
        let usize = intern(TyKind::Prim(PrimTy::Usize));
        let isize = intern(TyKind::Prim(PrimTy::Isize));
        let unit = intern(TyKind::Unit);
        let never = intern(TyKind::Never);
        let error = intern(TyKind::Error);
        drop(intern);
        Self {
            arena,
            interner,
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

    pub fn intern(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.interner.get(&kind) {
            return id;
        }
        let id = self.arena.push(kind.clone());
        self.interner.insert(kind, id);
        id
    }

    pub fn kind(&self, id: TyId) -> &TyKind {
        &self.arena[id]
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
            TyKind::Error => "{error}".to_string(),
        }
    }
}
