use std::fmt::Write;

use index_vec::IndexVec;

use super::ast::*;
use crate::loader::LoadedFile;
use crate::reporter::{FileId, SourceMap};

/// Tree-shaped renderer of a `Module`. Walks the arenas from `root_items`,
/// dereferencing IDs inline so the output reads like a syntax tree rather
/// than a flat dump.
pub fn pretty_print(module: &Module) -> String {
    let mut out = String::new();
    let mut p = Printer { out: &mut out, m: module, indent: 0 };
    p.write_line("Module");
    for &iid in &module.root_items {
        p.indent += 1;
        p.print_item(iid);
        p.indent -= 1;
    }
    out
}

/// Multi-file variant: prints every loaded file under a
/// `Module <path>` header (looked up via `source_map`). The `root` file
/// is emitted first; remaining files follow in `FileId` order. Mirrors
/// the input shape of `lower_program`.
pub fn pretty_print_program(
    files: &IndexVec<FileId, LoadedFile>,
    source_map: &SourceMap,
    root: FileId,
) -> String {
    let mut out = String::new();

    let mut order: Vec<FileId> = vec![root];
    for (fid, _) in files.iter_enumerated() {
        if fid != root {
            order.push(fid);
        }
    }

    for fid in order {
        let module = &files[fid].ast;
        let path = source_map.get(fid).path.display();
        let header = if fid == root {
            format!("Module {} (root)", path)
        } else {
            format!("Module {}", path)
        };
        let mut p = Printer { out: &mut out, m: module, indent: 0 };
        p.write_line(&header);
        for &iid in &module.root_items {
            p.indent += 1;
            p.print_item(iid);
            p.indent -= 1;
        }
    }

    out
}

struct Printer<'a> {
    out: &'a mut String,
    m: &'a Module,
    indent: usize,
}

impl<'a> Printer<'a> {
    fn begin_line(&mut self) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
    }

    fn write(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn end_line(&mut self) {
        self.out.push('\n');
    }

    fn write_line(&mut self, s: &str) {
        self.begin_line();
        self.write(s);
        self.end_line();
    }

    fn print_item(&mut self, iid: ItemId) {
        let item = &self.m.items[iid];
        match &item.kind {
            ItemKind::Fn(f) => self.print_fn(f),
            ItemKind::ExternBlock(b) => {
                self.begin_line();
                self.write("ExternBlock ");
                write!(self.out, "{:?}", b.abi).unwrap();
                self.end_line();
                self.indent += 1;
                for &child_iid in &b.items {
                    self.print_item(child_iid);
                }
                self.indent -= 1;
            }
            ItemKind::Struct(s) => self.print_struct(s),
            ItemKind::Import(i) => {
                self.begin_line();
                self.write("Import ");
                write!(self.out, "{:?}", i.path).unwrap();
                self.end_line();
            }
        }
    }

    fn print_struct(&mut self, s: &StructDecl) {
        self.begin_line();
        self.write("Struct ");
        self.write(&s.name.name);
        self.end_line();
        self.indent += 1;
        for f in &s.fields {
            self.begin_line();
            self.write(&f.name.name);
            self.write(": ");
            self.write_type(f.ty);
            self.end_line();
        }
        self.indent -= 1;
    }

    fn print_fn(&mut self, f: &FnDecl) {
        self.begin_line();
        self.write("Fn ");
        self.write(&f.name.name);
        self.write("(");
        for (i, p) in f.params.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            if p.mutable {
                self.write("mut ");
            }
            self.write(&p.name.name);
            self.write(": ");
            self.write_type(p.ty);
        }
        if f.is_variadic {
            if !f.params.is_empty() {
                self.write(", ");
            }
            self.write("...");
        }
        self.write(")");
        if let Some(rt) = f.ret_ty {
            self.write(" -> ");
            self.write_type(rt);
        }
        match f.body {
            Some(bid) => {
                self.end_line();
                self.indent += 1;
                self.print_block(bid);
                self.indent -= 1;
            }
            None => {
                self.write(";");
                self.end_line();
            }
        }
    }

    fn print_block(&mut self, bid: BlockId) {
        let block = &self.m.blocks[bid];
        self.write_line("Block");
        self.indent += 1;
        let last_idx = block.items.len().checked_sub(1);
        for (i, item) in block.items.iter().enumerate() {
            let is_last = Some(i) == last_idx;
            self.print_block_item(item.expr, item.has_semi, is_last);
        }
        self.indent -= 1;
    }

    /// Render one block item. The last item with `has_semi == false`
    /// produces the block's value, so we mark it with `tail:`. Earlier
    /// items either ran with `;` (ordinary statement) or without (mid-
    /// block discarded — typeck rejects most of these unless they're
    /// `()`/`!`-typed). Multi-line shapes (`If`, `Block`) print in full
    /// regardless of position; one-liners use the `tail:`/`ExprStmt …`
    /// convention.
    fn print_block_item(&mut self, eid: ExprId, has_semi: bool, is_last: bool) {
        let kind = &self.m.exprs[eid].kind;
        let is_value_producing = is_last && !has_semi;
        match kind {
            ExprKind::If { .. }
            | ExprKind::Block(_)
            | ExprKind::While { .. }
            | ExprKind::Loop { .. }
            | ExprKind::For { .. } => {
                if is_value_producing {
                    self.write_line("tail:");
                    self.indent += 1;
                    self.print_expr(eid);
                    self.indent -= 1;
                } else {
                    self.print_expr(eid);
                }
            }
            ExprKind::Let { .. } | ExprKind::Return(_) => {
                self.begin_line();
                self.append_expr(eid);
                self.end_line();
            }
            _ => {
                let prefix = if is_value_producing {
                    "tail: "
                } else if !has_semi {
                    "Discarded "
                } else {
                    "ExprStmt "
                };
                self.begin_line();
                self.write(prefix);
                self.append_expr(eid);
                self.end_line();
            }
        }
    }

    fn print_else_arm(&mut self, arm: &ElseArm) {
        match arm {
            ElseArm::Block(bid) => self.print_block(*bid),
            ElseArm::If(eid) => self.print_expr(*eid),
        }
    }

    fn print_expr(&mut self, eid: ExprId) {
        let kind = &self.m.exprs[eid];
        match &kind.kind {
            ExprKind::If { cond, then_block, else_arm } => {
                let cond = *cond;
                let then_block = *then_block;
                let else_arm = else_arm.clone();
                self.begin_line();
                self.write("If ");
                self.append_expr(cond);
                self.end_line();
                self.indent += 1;
                self.write_line("then:");
                self.indent += 1;
                self.print_block(then_block);
                self.indent -= 1;
                if let Some(arm) = else_arm {
                    self.write_line("else:");
                    self.indent += 1;
                    self.print_else_arm(&arm);
                    self.indent -= 1;
                }
                self.indent -= 1;
            }
            ExprKind::Block(bid) => {
                let bid = *bid;
                self.print_block(bid);
            }
            ExprKind::While { cond, body } => {
                let cond = *cond;
                let body = *body;
                self.begin_line();
                self.write("While ");
                self.append_expr(cond);
                self.end_line();
                self.indent += 1;
                self.print_block(body);
                self.indent -= 1;
            }
            ExprKind::Loop { body } => {
                let body = *body;
                self.write_line("Loop");
                self.indent += 1;
                self.print_block(body);
                self.indent -= 1;
            }
            ExprKind::For {
                init,
                cond,
                update,
                body,
            } => {
                let init = *init;
                let cond = *cond;
                let update = *update;
                let body = *body;
                self.write_line("For");
                self.indent += 1;
                self.write_for_header_slot("init:", init);
                self.write_for_header_slot("cond:", cond);
                self.write_for_header_slot("update:", update);
                self.print_block(body);
                self.indent -= 1;
            }
            _ => {
                self.begin_line();
                self.append_expr(eid);
                self.end_line();
            }
        }
    }

    fn write_for_header_slot(&mut self, label: &str, slot: Option<ExprId>) {
        match slot {
            Some(eid) => {
                self.begin_line();
                self.write(label);
                self.write(" ");
                self.append_expr(eid);
                self.end_line();
            }
            None => self.write_line(&format!("{label} <empty>")),
        }
    }

    fn append_expr(&mut self, eid: ExprId) {
        let kind = self.m.exprs[eid].kind.clone();
        match &kind {
            ExprKind::IntLit(n) => write!(self.out, "Int({n})").unwrap(),
            ExprKind::BoolLit(b) => write!(self.out, "Bool({b})").unwrap(),
            ExprKind::CharLit(c) => write!(self.out, "Char({c:?})").unwrap(),
            ExprKind::StrLit(s) => write!(self.out, "Str({s:?})").unwrap(),
            ExprKind::Null => write!(self.out, "Null").unwrap(),
            ExprKind::Ident(id) => write!(self.out, "Ident({:?})", id.name).unwrap(),
            ExprKind::Paren(inner) => {
                self.write("(");
                self.append_expr(*inner);
                self.write(")");
            }
            ExprKind::Unary { op, expr } => {
                write!(self.out, "Unary({op:?}, ").unwrap();
                self.append_expr(*expr);
                self.write(")");
            }
            ExprKind::Binary { op, lhs, rhs } => {
                write!(self.out, "Binary({op:?}, ").unwrap();
                self.append_expr(*lhs);
                self.write(", ");
                self.append_expr(*rhs);
                self.write(")");
            }
            ExprKind::Assign { op, lhs, rhs } => {
                write!(self.out, "Assign({op:?}, ").unwrap();
                self.append_expr(*lhs);
                self.write(", ");
                self.append_expr(*rhs);
                self.write(")");
            }
            ExprKind::Call { callee, args } => {
                self.append_expr(*callee);
                self.write("(");
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.append_expr(*a);
                }
                self.write(")");
            }
            ExprKind::Index { base, index } => {
                self.append_expr(*base);
                self.write("[");
                self.append_expr(*index);
                self.write("]");
            }
            ExprKind::Field { base, name } => {
                self.append_expr(*base);
                self.write(".");
                self.write(&name.name);
            }
            ExprKind::StructLit { name, fields } => {
                self.write("StructLit ");
                self.write(&name.name);
                self.write(" {");
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(" ");
                    self.write(&f.name.name);
                    self.write(": ");
                    self.append_expr(f.value);
                }
                if !fields.is_empty() {
                    self.write(" ");
                }
                self.write("}");
            }
            ExprKind::ArrayLit(lit) => match lit {
                ArrayLit::Elems(es) => {
                    self.write("[");
                    for (i, eid) in es.iter().enumerate() {
                        if i > 0 {
                            self.write(", ");
                        }
                        self.append_expr(*eid);
                    }
                    self.write("]");
                }
                ArrayLit::Repeat { init, len } => {
                    self.write("[");
                    self.append_expr(*init);
                    self.write("; ");
                    self.append_expr(*len);
                    self.write("]");
                }
            },
            ExprKind::Cast { expr, ty } => {
                self.append_expr(*expr);
                self.write(" as ");
                self.write_type(*ty);
            }
            ExprKind::AddrOf { mutability, expr } => {
                self.write("&");
                if *mutability == Mutability::Mut {
                    self.write("mut ");
                }
                self.append_expr(*expr);
            }
            ExprKind::Let { mutable, name, ty, init } => {
                self.write("Let ");
                if *mutable {
                    self.write("mut ");
                }
                self.write(&name.name);
                if let Some(t) = ty {
                    self.write(": ");
                    self.write_type(*t);
                }
                if let Some(init) = init {
                    self.write(" = ");
                    self.append_expr(*init);
                }
            }
            ExprKind::Return(val) => {
                self.write("Return");
                if let Some(eid) = val {
                    self.write(" ");
                    self.append_expr(*eid);
                }
            }
            ExprKind::Break { expr } => {
                self.write("Break");
                if let Some(eid) = expr {
                    self.write(" ");
                    self.append_expr(*eid);
                }
            }
            ExprKind::Continue => self.write("Continue"),
            ExprKind::If { .. } => self.write("If(…)"),
            ExprKind::Block(_) => self.write("Block(…)"),
            // While/Loop/For are routed through `print_expr`'s multi-line
            // arms via the `print_block_item` dispatch; reaching them
            // here means the parent code called `append_expr` on a loop
            // expression (rare — happens only if a loop appears nested
            // inside a single-line expression context, e.g. as an
            // operand of `+`). Print a placeholder rather than the
            // whole tree to keep the inline shape sane.
            ExprKind::While { .. } => self.write("While(…)"),
            ExprKind::Loop { .. } => self.write("Loop(…)"),
            ExprKind::For { .. } => self.write("For(…)"),
            ExprKind::Poison => self.write("<poison>"),
        }
    }

    fn write_type(&mut self, tid: TypeId) {
        let kind = self.m.types[tid].kind.clone();
        match &kind {
            TypeKind::Named(id) => self.write(&id.name),
            TypeKind::Ptr { mutability, pointee } => {
                self.write("*");
                self.write(mutability.as_str());
                self.write(" ");
                self.write_type(*pointee);
            }
            TypeKind::Array { elem, len } => {
                self.write("[");
                self.write_type(*elem);
                if let Some(eid) = len {
                    self.write("; ");
                    self.append_expr(*eid);
                }
                self.write("]");
            }
        }
    }
}
