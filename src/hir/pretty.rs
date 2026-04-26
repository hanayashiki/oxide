use std::fmt::Write;

use super::ir::*;

/// Tree-shaped renderer of a `HirModule`. Walks from `root_adts` then
/// `root_fns`, resolving IDs inline. Local references show their
/// `LocalId`, Fn references show their `FnId`, ADT references show
/// their `HAdtId`, so the user can confirm name resolution at a glance.
///
/// Inline expression rendering uses a uniform `Name(arg1, arg2, …, place?)`
/// form. The optional `place` arg appears as the last positional argument
/// when `HirExpr::is_place == true`. Block-as-expression is the one
/// exception: it renders as `{ item1; item2; tail }` (no `Name` prefix)
/// since braces already disambiguate and blocks are never places.
pub fn pretty_print(module: &HirModule) -> String {
    let mut out = String::new();
    let mut p = Printer { out: &mut out, m: module, indent: 0 };
    p.write_line("HirModule");
    for &haid in &module.root_adts {
        p.indent += 1;
        p.print_adt(haid);
        p.indent -= 1;
    }
    for &fid in &module.root_fns {
        p.indent += 1;
        p.print_fn(fid);
        p.indent -= 1;
    }
    out
}

struct Printer<'a> {
    out: &'a mut String,
    m: &'a HirModule,
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

    fn print_adt(&mut self, haid: HAdtId) {
        let adt = &self.m.adts[haid];
        let kind = match adt.kind {
            AdtKind::Struct => "Struct",
        };
        self.write_line(&format!("{}[{}] {}", kind, haid.raw(), adt.name));
        self.indent += 1;
        for v in adt.variants.iter() {
            // Structs use the implicit unnamed variant; for them, just
            // dump fields directly. Named variants (future enums) get a
            // wrapper line.
            if let Some(name) = &v.name {
                self.write_line(&format!("Variant {}", name));
                self.indent += 1;
                for f in v.fields.iter() {
                    self.write_line(&format!("{}: {}", f.name, ty_str(&f.ty)));
                }
                self.indent -= 1;
            } else {
                for f in v.fields.iter() {
                    self.write_line(&format!("{}: {}", f.name, ty_str(&f.ty)));
                }
            }
        }
        self.indent -= 1;
    }

    fn print_fn(&mut self, fid: FnId) {
        let f = &self.m.fns[fid];
        let prefix = if f.is_extern { "ExternFn" } else { "Fn" };
        let mut header = format!("{}[{}] {}(", prefix, fid.raw(), f.name);
        for (i, &lid) in f.params.iter().enumerate() {
            if i > 0 {
                header.push_str(", ");
            }
            let local = &self.m.locals[lid];
            write!(header, "{}[Local({})]", local.name, lid.raw()).unwrap();
            if let Some(ty) = &local.ty {
                write!(header, ": {}", ty_str(ty)).unwrap();
            }
        }
        header.push(')');
        if let Some(rt) = &f.ret_ty {
            write!(header, " -> {}", ty_str(rt)).unwrap();
        }
        self.write_line(&header);
        if let Some(bid) = f.body {
            self.indent += 1;
            self.print_block(bid);
            self.indent -= 1;
        }
    }

    fn print_block(&mut self, bid: HBlockId) {
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

    /// Same convention as the parser pretty: the last item with
    /// `has_semi == false` is rendered with a `tail:` prefix; earlier
    /// items either ran with `;` (ordinary statement) or without
    /// (`Discarded` — typeck-validated against `()` or `!`).
    fn print_block_item(&mut self, eid: HExprId, has_semi: bool, is_last: bool) {
        let kind = &self.m.exprs[eid].kind;
        let is_value_producing = is_last && !has_semi;
        match kind {
            HirExprKind::If { .. } | HirExprKind::Block(_) => {
                if is_value_producing {
                    self.write_line("tail:");
                    self.indent += 1;
                    self.print_expr(eid);
                    self.indent -= 1;
                } else {
                    self.print_expr(eid);
                }
            }
            HirExprKind::Let { .. } | HirExprKind::Return(_) => {
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

    fn print_else_arm(&mut self, arm: &HElseArm) {
        match arm {
            HElseArm::Block(bid) => self.print_block(*bid),
            HElseArm::If(eid) => self.print_expr(*eid),
        }
    }

    fn print_expr(&mut self, eid: HExprId) {
        let expr = &self.m.exprs[eid];
        match &expr.kind {
            HirExprKind::If { cond, then_block, else_arm } => {
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
            HirExprKind::Block(bid) => self.print_block(*bid),
            _ => {
                let mut buf = String::new();
                self.append_expr(&mut buf, eid);
                self.write_line(&buf);
            }
        }
    }

    /// Render an expression inline.
    ///
    /// Uniform structure: `Name(arg1, arg2, …, place?)`, where the optional
    /// `place` is the last positional arg when `expr.is_place == true`.
    /// `Block` is the lone exception — renders as `{ items }` since braces
    /// already delimit, and blocks are never places.
    fn append_expr(&self, buf: &mut String, eid: HExprId) {
        let expr = &self.m.exprs[eid];

        // Block-as-expression: no Name prefix, just the inline brace form.
        if let HirExprKind::Block(bid) = &expr.kind {
            self.append_block_inline(buf, *bid);
            return;
        }

        let args = self.expr_args(eid);
        let name = expr_name(&expr.kind);

        write!(buf, "{}(", name).unwrap();
        let mut first = true;
        for arg in &args {
            if !first {
                buf.push_str(", ");
            }
            buf.push_str(arg);
            first = false;
        }
        if expr.is_place {
            if !first {
                buf.push_str(", ");
            }
            buf.push_str("place");
        }
        buf.push(')');
    }

    /// Build the positional-argument list for an expression (excluding the
    /// optional `place` marker, which is appended uniformly by `append_expr`).
    /// Sub-expressions are rendered recursively into strings; sub-blocks
    /// render as `{ items }`.
    fn expr_args(&self, eid: HExprId) -> Vec<String> {
        let expr = &self.m.exprs[eid];
        match &expr.kind {
            HirExprKind::IntLit(n) => vec![n.to_string()],
            HirExprKind::BoolLit(b) => vec![b.to_string()],
            HirExprKind::CharLit(c) => vec![c.to_string()],
            HirExprKind::StrLit(s) => vec![format!("{s:?}")],
            HirExprKind::Local(lid) => {
                let name = &self.m.locals[*lid].name;
                vec![lid.raw().to_string(), format!("{name:?}")]
            }
            HirExprKind::Fn(fid) => {
                let name = &self.m.fns[*fid].name;
                vec![fid.raw().to_string(), format!("{name:?}")]
            }
            HirExprKind::Unresolved(name) => vec![format!("{name:?}")],
            HirExprKind::Unary { op, expr: sub } => {
                vec![format!("{op:?}"), self.render_expr(*sub)]
            }
            HirExprKind::Binary { op, lhs, rhs } => vec![
                format!("{op:?}"),
                self.render_expr(*lhs),
                self.render_expr(*rhs),
            ],
            HirExprKind::Assign { op, target, rhs } => vec![
                format!("{op:?}"),
                self.render_expr(*target),
                self.render_expr(*rhs),
            ],
            HirExprKind::Call { callee, args } => {
                let mut out = Vec::with_capacity(1 + args.len());
                out.push(self.render_expr(*callee));
                for a in args {
                    out.push(self.render_expr(*a));
                }
                out
            }
            HirExprKind::Index { base, index } => {
                vec![self.render_expr(*base), self.render_expr(*index)]
            }
            HirExprKind::Field { base, name } => {
                vec![self.render_expr(*base), format!("{name:?}")]
            }
            HirExprKind::StructLit { adt, fields } => {
                let adt_name = &self.m.adts[*adt].name;
                let mut out = Vec::with_capacity(2 + fields.len());
                out.push(adt.raw().to_string());
                out.push(format!("{adt_name:?}"));
                for f in fields {
                    out.push(format!("{}: {}", f.name, self.render_expr(f.value)));
                }
                out
            }
            HirExprKind::Cast { expr: sub, ty } => {
                vec![self.render_expr(*sub), ty_str(ty)]
            }
            HirExprKind::If { cond, then_block, else_arm } => {
                let mut out = vec![self.render_expr(*cond), self.render_block_inline(*then_block)];
                if let Some(arm) = else_arm {
                    out.push(match arm {
                        HElseArm::Block(bid) => self.render_block_inline(*bid),
                        HElseArm::If(eid) => self.render_expr(*eid),
                    });
                }
                out
            }
            HirExprKind::Block(_) => {
                // Block has its own brace-form rendering and is handled
                // before `expr_args` is reached.
                unreachable!("Block handled by append_expr's early return")
            }
            HirExprKind::Return(val) => match val {
                Some(eid) => vec![self.render_expr(*eid)],
                None => vec![],
            },
            HirExprKind::Let { local, init } => {
                let l = &self.m.locals[*local];
                let mut out = Vec::new();
                out.push(local.raw().to_string());
                out.push(format!("{:?}", l.name));
                if l.mutable {
                    out.push("mut".to_string());
                }
                if let Some(ty) = &l.ty {
                    out.push(format!(":{}", ty_str(ty)));
                }
                if let Some(init) = init {
                    out.push(self.render_expr(*init));
                }
                out
            }
            HirExprKind::Poison => vec![],
        }
    }

    fn render_expr(&self, eid: HExprId) -> String {
        let mut buf = String::new();
        self.append_expr(&mut buf, eid);
        buf
    }

    /// Inline `{ item1; item2; tail }` rendering for a block. The trailing
    /// `;` matches the source-level `has_semi` of each item — the tail
    /// (last item with `has_semi == false`) has no `;` after it.
    fn append_block_inline(&self, buf: &mut String, bid: HBlockId) {
        let block = &self.m.blocks[bid];
        if block.items.is_empty() {
            buf.push_str("{}");
            return;
        }
        buf.push_str("{ ");
        for (i, item) in block.items.iter().enumerate() {
            if i > 0 {
                buf.push(' ');
            }
            self.append_expr(buf, item.expr);
            if item.has_semi {
                buf.push(';');
            }
        }
        buf.push_str(" }");
    }

    fn render_block_inline(&self, bid: HBlockId) -> String {
        let mut buf = String::new();
        self.append_block_inline(&mut buf, bid);
        buf
    }
}

fn expr_name(kind: &HirExprKind) -> &'static str {
    match kind {
        HirExprKind::IntLit(_) => "Int",
        HirExprKind::BoolLit(_) => "Bool",
        HirExprKind::CharLit(_) => "Char",
        HirExprKind::StrLit(_) => "Str",
        HirExprKind::Local(_) => "Local",
        HirExprKind::Fn(_) => "Fn",
        HirExprKind::Unresolved(_) => "Unresolved",
        HirExprKind::Unary { .. } => "Unary",
        HirExprKind::Binary { .. } => "Binary",
        HirExprKind::Assign { .. } => "Assign",
        HirExprKind::Call { .. } => "Call",
        HirExprKind::Index { .. } => "Index",
        HirExprKind::Field { .. } => "Field",
        HirExprKind::StructLit { .. } => "StructLit",
        HirExprKind::Cast { .. } => "Cast",
        HirExprKind::If { .. } => "If",
        HirExprKind::Block(_) => "Block",
        HirExprKind::Return(_) => "Return",
        HirExprKind::Let { .. } => "Let",
        HirExprKind::Poison => "Poison",
    }
}

fn ty_str(ty: &HirTy) -> String {
    match &ty.kind {
        HirTyKind::Named(name) => name.clone(),
        HirTyKind::Adt(haid) => format!("Adt({})", haid.raw()),
        HirTyKind::Ptr { mutability, pointee } => {
            format!("*{} {}", mutability.as_str(), ty_str(pointee))
        }
        HirTyKind::Error => "<err>".to_string(),
    }
}
