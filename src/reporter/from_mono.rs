use super::diagnostic::{Diagnostic, Label};
use super::source_map::FileId;
use crate::hir::HirProgram;
use crate::mono::MonoError;
use crate::typeck::TyArena;

/// Render a mono error to a structured diagnostic. Currently the only
/// variant is E0278 (DivergentMonomorphization); the chain renders via
/// `TyArena::render` for each type-arg so a human reads the same form
/// they'd see in any other typeck-side diagnostic. See
/// spec/16_GENERIC.md §Errors.
pub fn from_mono_error(
    err: &MonoError,
    file: FileId,
    hir: &HirProgram,
    tys: &TyArena,
) -> Diagnostic {
    match err {
        MonoError::DivergentMonomorphization { chain, span, limit } => {
            // Cap rendered type width: a divergent recursion can
            // produce types with hundreds of `*mut` layers, and
            // emitting them in full bloats the diagnostic to hundreds
            // of KB. Truncate from the middle to keep both ends
            // readable.
            const TYPE_RENDER_MAX: usize = 80;
            fn truncate_middle(s: &str, max: usize) -> String {
                if s.len() <= max {
                    return s.to_string();
                }
                let head = max / 2 - 2;
                let tail = max - head - 5; // "...".len() + spacing
                let mut out = String::with_capacity(max);
                out.push_str(&s[..head]);
                out.push_str(" ... ");
                out.push_str(&s[s.len() - tail..]);
                out
            }

            // Render one chain entry as `<fnname><<arg1>, <arg2>, ...>`
            // with each type-arg going through TyArena::render.
            let render_entry = |(fid, type_args, _span): &(
                crate::hir::FnId,
                Vec<crate::typeck::TyId>,
                crate::reporter::Span,
            )| {
                let mut s = hir.fns[*fid].name.clone();
                if !type_args.is_empty() {
                    s.push('<');
                    for (j, &t) in type_args.iter().enumerate() {
                        if j > 0 {
                            s.push_str(", ");
                        }
                        s.push_str(&truncate_middle(&tys.render(t), TYPE_RENDER_MAX));
                    }
                    s.push('>');
                }
                s
            };

            // Truncate huge chains. A divergent type doubles in size at
            // each step, so 256 entries renders to a multi-hundred-KB
            // help block — useless. Show first HEAD and last TAIL with
            // an elision in between when the chain exceeds HEAD+TAIL.
            const HEAD: usize = 5;
            const TAIL: usize = 5;
            let mut help = String::from("instantiation chain (root → tip):\n");
            if chain.len() <= HEAD + TAIL {
                for (i, entry) in chain.iter().enumerate() {
                    help.push_str(&format!("  {}  (depth {i})\n", render_entry(entry)));
                }
            } else {
                for (i, entry) in chain.iter().take(HEAD).enumerate() {
                    help.push_str(&format!("  {}  (depth {i})\n", render_entry(entry)));
                }
                let elided = chain.len() - HEAD - TAIL;
                help.push_str(&format!("  ... ({elided} entries elided)\n"));
                let tail_start = chain.len() - TAIL;
                for (offset, entry) in chain.iter().skip(tail_start).enumerate() {
                    let i = tail_start + offset;
                    help.push_str(&format!("  {}  (depth {i})\n", render_entry(entry)));
                }
            }
            help.push_str(
                "each step discovers a strictly-larger type — dedup never converges. \
                 Restructure the recursion to use a fixed type parameter or factor \
                 through a non-generic helper.",
            );
            Diagnostic::error(
                "E0278",
                format!("monomorphization depth exceeded (limit: {limit})"),
            )
            .with_label(Label::primary(file, span.clone(), "diverges here"))
            .with_help(help)
        }
    }
}
