//! Type vocabulary: hash-cons interner (`TyArena`) and the `TyKind` enum.
//!
//! Equal types share a `TyId` because every construction goes through
//! `TyArena::intern`. Codegen and typeck can compare types via
//! `id == id` instead of walking structures.

use std::collections::HashMap;

use index_vec::IndexVec;

use crate::parser::ast::Mutability;

index_vec::define_index_type! { pub struct TyId    = u32; }
index_vec::define_index_type! { pub struct InferId = u32; }

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum TyKind {
    Prim(PrimTy),
    /// `()` — zero-sized.
    Unit,
    /// `!` — bottom type. Subtype of every type during unification.
    Never,
    /// Function signature.
    Fn(Vec<TyId>, TyId),
    /// `*const T` / `*mut T`. Mutability is interned alongside the pointee
    /// so the arena distinguishes the two variants. Unify treats them
    /// equivalently (shape only); the coercion check at use sites enforces
    /// the actual `mut → const` direction rule. See `spec/07_POINTER.md`.
    Ptr(TyId, Mutability),
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
            TyKind::Fn(params, ret) => {
                let mut s = String::from("fn(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&self.render(*p));
                }
                s.push(')');
                if *ret != self.unit {
                    s.push_str(" -> ");
                    s.push_str(&self.render(*ret));
                }
                s
            }
            TyKind::Ptr(inner, m) => format!("*{} {}", m.as_str(), self.render(*inner)),
            TyKind::Infer(id) => format!("?T{}", id.raw()),
            TyKind::Error => "{error}".to_string(),
        }
    }
}
