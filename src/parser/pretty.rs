use std::fmt::Write;

use super::ast::*;

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

struct Printer<'a> {
    out: &'a mut String,
    m: &'a Module,
    indent: usize,
}

impl<'a> Printer<'a> {
    fn write_line(&mut self, s: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn print_item(&mut self, iid: ItemId) {
        let item = &self.m.items[iid];
        match &item.kind {
            ItemKind::Fn(f) => self.print_fn(f),
            ItemKind::ExternBlock(b) => {
                self.write_line(&format!("ExternBlock {:?}", b.abi));
                self.indent += 1;
                for f in &b.items {
                    self.print_fn(f);
                }
                self.indent -= 1;
            }
            ItemKind::Struct(s) => self.print_struct(s),
        }
    }

    fn print_struct(&mut self, s: &StructDecl) {
        self.write_line(&format!("Struct {}", s.name.name));
        self.indent += 1;
        for f in &s.fields {
            self.write_line(&format!("{}: {}", f.name.name, type_str(self.m, f.ty)));
        }
        self.indent -= 1;
    }

    fn print_fn(&mut self, f: &FnDecl) {
        let mut header = format!("Fn {}(", f.name.name);
        for (i, p) in f.params.iter().enumerate() {
            if i > 0 {
                header.push_str(", ");
            }
            if p.mutable {
                header.push_str("mut ");
            }
            write!(header, "{}: {}", p.name.name, type_str(self.m, p.ty)).unwrap();
        }
        header.push(')');
        if let Some(rt) = f.ret_ty {
            write!(header, " -> {}", type_str(self.m, rt)).unwrap();
        }
        match f.body {
            Some(bid) => {
                self.write_line(&header);
                self.indent += 1;
                self.print_block(bid);
                self.indent -= 1;
            }
            None => {
                header.push_str(";");
                self.write_line(&header);
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
            ExprKind::If { .. } | ExprKind::Block(_) => {
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
                let mut buf = String::new();
                self.append_expr(&mut buf, eid);
                self.write_line(&buf);
            }
            _ => {
                let prefix = if is_value_producing {
                    "tail: "
                } else if !has_semi {
                    "Discarded "
                } else {
                    "ExprStmt "
                };
                let mut buf = String::from(prefix);
                self.append_expr(&mut buf, eid);
                self.write_line(&buf);
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
        let expr = &self.m.exprs[eid];
        match &expr.kind {
            ExprKind::If { cond, then_block, else_arm } => {
                let mut buf = String::from("If ");
                self.append_expr(&mut buf, *cond);
                self.write_line(&buf);
                self.indent += 1;
                self.write_line("then:");
                self.indent += 1;
                self.print_block(*then_block);
                self.indent -= 1;
                if let Some(arm) = else_arm {
                    self.write_line("else:");
                    self.indent += 1;
                    self.print_else_arm(arm);
                    self.indent -= 1;
                }
                self.indent -= 1;
            }
            ExprKind::Block(bid) => self.print_block(*bid),
            _ => {
                let mut buf = String::new();
                self.append_expr(&mut buf, eid);
                self.write_line(&buf);
            }
        }
    }

    fn append_expr(&self, buf: &mut String, eid: ExprId) {
        let expr = &self.m.exprs[eid];
        match &expr.kind {
            ExprKind::IntLit(n) => write!(buf, "Int({n})").unwrap(),
            ExprKind::BoolLit(b) => write!(buf, "Bool({b})").unwrap(),
            ExprKind::CharLit(c) => write!(buf, "Char({c:?})").unwrap(),
            ExprKind::StrLit(s) => write!(buf, "Str({s:?})").unwrap(),
            ExprKind::Ident(id) => write!(buf, "Ident({:?})", id.name).unwrap(),
            ExprKind::Paren(inner) => {
                buf.push('(');
                self.append_expr(buf, *inner);
                buf.push(')');
            }
            ExprKind::Unary { op, expr } => {
                write!(buf, "Unary({op:?}, ").unwrap();
                self.append_expr(buf, *expr);
                buf.push(')');
            }
            ExprKind::Binary { op, lhs, rhs } => {
                write!(buf, "Binary({op:?}, ").unwrap();
                self.append_expr(buf, *lhs);
                buf.push_str(", ");
                self.append_expr(buf, *rhs);
                buf.push(')');
            }
            ExprKind::Assign { op, lhs, rhs } => {
                write!(buf, "Assign({op:?}, ").unwrap();
                self.append_expr(buf, *lhs);
                buf.push_str(", ");
                self.append_expr(buf, *rhs);
                buf.push(')');
            }
            ExprKind::Call { callee, args } => {
                self.append_expr(buf, *callee);
                buf.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    self.append_expr(buf, *a);
                }
                buf.push(')');
            }
            ExprKind::Index { base, index } => {
                self.append_expr(buf, *base);
                buf.push('[');
                self.append_expr(buf, *index);
                buf.push(']');
            }
            ExprKind::Field { base, name } => {
                self.append_expr(buf, *base);
                write!(buf, ".{}", name.name).unwrap();
            }
            ExprKind::StructLit { name, fields } => {
                write!(buf, "StructLit {} {{", name.name).unwrap();
                for (i, f) in fields.iter().enumerate() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    write!(buf, " {}: ", f.name.name).unwrap();
                    self.append_expr(buf, f.value);
                }
                if !fields.is_empty() {
                    buf.push(' ');
                }
                buf.push('}');
            }
            ExprKind::ArrayLit(lit) => match lit {
                ArrayLit::Elems(es) => {
                    buf.push('[');
                    for (i, eid) in es.iter().enumerate() {
                        if i > 0 {
                            buf.push_str(", ");
                        }
                        self.append_expr(buf, *eid);
                    }
                    buf.push(']');
                }
                ArrayLit::Repeat { init, len } => {
                    buf.push('[');
                    self.append_expr(buf, *init);
                    buf.push_str("; ");
                    self.append_expr(buf, *len);
                    buf.push(']');
                }
            },
            ExprKind::Cast { expr, ty } => {
                self.append_expr(buf, *expr);
                write!(buf, " as {}", type_str(self.m, *ty)).unwrap();
            }
            ExprKind::AddrOf { mutability, expr } => {
                buf.push('&');
                if *mutability == Mutability::Mut {
                    buf.push_str("mut ");
                }
                self.append_expr(buf, *expr);
            }
            ExprKind::Let { mutable, name, ty, init } => {
                buf.push_str("Let ");
                if *mutable {
                    buf.push_str("mut ");
                }
                buf.push_str(&name.name);
                if let Some(t) = ty {
                    write!(buf, ": {}", type_str(self.m, *t)).unwrap();
                }
                if let Some(init) = init {
                    buf.push_str(" = ");
                    self.append_expr(buf, *init);
                }
            }
            ExprKind::Return(val) => {
                buf.push_str("Return");
                if let Some(eid) = val {
                    buf.push(' ');
                    self.append_expr(buf, *eid);
                }
            }
            ExprKind::If { .. } => buf.push_str("If(…)"),
            ExprKind::Block(_) => buf.push_str("Block(…)"),
            ExprKind::Poison => buf.push_str("<poison>"),
        }
    }
}

fn type_str(m: &Module, tid: TypeId) -> String {
    let t = &m.types[tid];
    match &t.kind {
        TypeKind::Named(id) => id.name.clone(),
        TypeKind::Ptr { mutability, pointee } => {
            format!("*{} {}", mutability.as_str(), type_str(m, *pointee))
        }
        TypeKind::Array { elem, len } => match len {
            None => format!("[{}]", type_str(m, *elem)),
            Some(eid) => {
                let mut buf = format!("[{}; ", type_str(m, *elem));
                append_expr_to_buf(m, &mut buf, *eid);
                buf.push(']');
                buf
            }
        },
    }
}

/// Render an expression for the type-printer's needs (used by `[T; N]` length
/// rendering). Re-uses Printer's `append_expr` shape but is standalone since
/// `type_str` is a free function. The `Printer::out` is unused by
/// `append_expr` (it writes to the passed `buf`), so we give it a sink
/// `String` to satisfy the borrow.
fn append_expr_to_buf(m: &Module, buf: &mut String, eid: ExprId) {
    let mut sink = String::new();
    let p = Printer { out: &mut sink, m, indent: 0 };
    p.append_expr(buf, eid);
}
