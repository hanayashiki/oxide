use std::fmt::Write;

use super::ir::*;

/// Tree-shaped renderer of a `HirModule`. Walks from `root_fns`,
/// resolving IDs inline. Local references show their `LocalId`,
/// Fn references show their `FnId`, so the user can confirm name
/// resolution at a glance.
pub fn pretty_print(module: &HirModule) -> String {
    let mut out = String::new();
    let mut p = Printer { out: &mut out, m: module, indent: 0 };
    p.write_line("HirModule");
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
        for &eid in &block.items {
            self.print_block_item(eid);
        }
        if let Some(eid) = block.tail {
            let mut buf = String::from("tail: ");
            self.append_expr(&mut buf, eid);
            self.write_line(&buf);
        }
        self.indent -= 1;
    }

    /// Block items dispatch on the contained expression's shape so
    /// multi-line forms (`If`, bare `Block`) stay indented; the rest
    /// render as one-liner `ExprStmt …` (or distinctive single-line
    /// forms for `Let`/`Return`).
    fn print_block_item(&mut self, eid: HExprId) {
        let kind = &self.m.exprs[eid].kind;
        match kind {
            HirExprKind::If { .. } | HirExprKind::Block(_) => self.print_expr(eid),
            HirExprKind::Let { .. } | HirExprKind::Return(_) => {
                let mut buf = String::new();
                self.append_expr(&mut buf, eid);
                self.write_line(&buf);
            }
            _ => {
                let mut buf = String::from("ExprStmt ");
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

    fn append_expr(&self, buf: &mut String, eid: HExprId) {
        let expr = &self.m.exprs[eid];
        match &expr.kind {
            HirExprKind::IntLit(n) => write!(buf, "Int({n})").unwrap(),
            HirExprKind::BoolLit(b) => write!(buf, "Bool({b})").unwrap(),
            HirExprKind::CharLit(b) => write!(buf, "Char({b})").unwrap(),
            HirExprKind::StrLit(s) => write!(buf, "Str({s:?})").unwrap(),
            HirExprKind::Local(lid) => {
                let name = &self.m.locals[*lid].name;
                write!(buf, "Local({}, \"{}\")", lid.raw(), name).unwrap();
            }
            HirExprKind::Fn(fid) => {
                let name = &self.m.fns[*fid].name;
                write!(buf, "Fn({}, \"{}\")", fid.raw(), name).unwrap();
            }
            HirExprKind::Unresolved(name) => {
                write!(buf, "Unresolved({:?})", name).unwrap();
            }
            HirExprKind::Unary { op, expr } => {
                write!(buf, "Unary({op:?}, ").unwrap();
                self.append_expr(buf, *expr);
                buf.push(')');
            }
            HirExprKind::Binary { op, lhs, rhs } => {
                write!(buf, "Binary({op:?}, ").unwrap();
                self.append_expr(buf, *lhs);
                buf.push_str(", ");
                self.append_expr(buf, *rhs);
                buf.push(')');
            }
            HirExprKind::Assign { op, target, rhs } => {
                write!(buf, "Assign({op:?}, ").unwrap();
                self.append_expr(buf, *target);
                buf.push_str(", ");
                self.append_expr(buf, *rhs);
                buf.push(')');
            }
            HirExprKind::Call { callee, args } => {
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
            HirExprKind::Index { base, index } => {
                self.append_expr(buf, *base);
                buf.push('[');
                self.append_expr(buf, *index);
                buf.push(']');
            }
            HirExprKind::Field { base, name } => {
                self.append_expr(buf, *base);
                write!(buf, ".{}", name).unwrap();
            }
            HirExprKind::Cast { expr, ty } => {
                self.append_expr(buf, *expr);
                write!(buf, " as {}", ty_str(ty)).unwrap();
            }
            HirExprKind::Let { local, init } => {
                let l = &self.m.locals[*local];
                buf.push_str("Let ");
                if l.mutable {
                    buf.push_str("mut ");
                }
                write!(buf, "{}[Local({})]", l.name, local.raw()).unwrap();
                if let Some(ty) = &l.ty {
                    write!(buf, ": {}", ty_str(ty)).unwrap();
                }
                if let Some(init) = init {
                    buf.push_str(" = ");
                    self.append_expr(buf, *init);
                }
            }
            HirExprKind::Return(val) => {
                buf.push_str("Return");
                if let Some(eid) = val {
                    buf.push(' ');
                    self.append_expr(buf, *eid);
                }
            }
            HirExprKind::If { .. } => buf.push_str("If(…)"),
            HirExprKind::Block(_) => buf.push_str("Block(…)"),
            HirExprKind::Poison => buf.push_str("<poison>"),
        }
    }
}

fn ty_str(ty: &HirTy) -> String {
    match &ty.kind {
        HirTyKind::Named(name) => name.clone(),
        HirTyKind::Error => "<err>".to_string(),
    }
}
