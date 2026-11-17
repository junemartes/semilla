//! The source ranges of the MIR elements of a body (design 40.1).
//!
//! An element is a statement or a terminator of the canonical body, in the order of the
//! canonical text: each block in order, its statements, then its terminator. The record gives
//! each element its source range: the file and the start and end byte offsets in the file as
//! written. rustc removes a BOM and changes each CRLF to LF before it reads a file, so the
//! driver maps each position back to the file as written (`SourceFile::normalized_pos`).
//!
//! A span from a macro expansion gets two ranges when they exist: the range of the tokens in
//! the source (a macro input, or the body of a `macro_rules` definition in the workspace), and
//! the range of the outermost macro call. A span in a different crate outside the workspace or
//! in the standard library has no source range.

use rustc_public::mir::Body;
use rustc_span::{ExpnKind, Span};

/// The ranges of the elements of one body. A run of elements with the same ranges is one
/// entry: `[first, count, file, start, end]`, with `[call file, call start, call end]` at the
/// end for a span from a macro call. `file` is `-1` if the tokens have no source range. An
/// element with no range has no entry.
#[derive(serde::Serialize, Default)]
pub struct Elements {
    /// The count of the elements of the body.
    pub n: usize,
    /// The files of the ranges (relative to the workspace root).
    pub f: Vec<String>,
    /// The runs of elements with the same ranges.
    pub r: Vec<Vec<i64>>,
    /// The symbol table of the body (design 44): the def path, the file (an index into `f`),
    /// and the byte range of the definition.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub s: Vec<crate::symbols::Symbol>,
    /// The symbols of the elements: runs `[first, count, symbol…]` of elements with the same
    /// symbol list. An element with no symbol has no entry.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub y: Vec<Vec<u32>>,
    /// The version of the symbol data: 1 for a body with symbol data (design 44).
    #[serde(skip_serializing_if = "is_zero")]
    pub v: u8,
    /// The index of the symbol of the function of the body in `s` (design 44).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub o: Option<u32>,
    /// True for a body that rustc evaluates at compile time (a constant, a static, a constant
    /// block): its symbols are evaluated, so the body of a `const fn` counts (design 44).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub c: bool,
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

/// One source range: the index of the file, the start, and the end.
type Range = (i64, i64, i64);

/// Returns the ranges of the elements of a canonical body, and the symbols of each element of
/// the body of `item` (design 44).
pub fn elements(body: &Body, item: Option<rustc_public::DefId>) -> Elements {
    let mut out = Elements::default();
    let spans: Vec<rustc_public::ty::Span> = body
        .blocks
        .iter()
        .flat_map(|block| block.statements.iter().map(|statement| statement.source_info.span).chain(std::iter::once(block.terminator.source_info.span)))
        .collect();
    out.n = spans.len();
    rustc_middle::ty::tls::with(|tcx| {
        let mut last: Option<(Option<Range>, Option<Range>)> = None;
        for (index, span) in spans.into_iter().enumerate() {
            let span = rustc_public::rustc_internal::internal(tcx, span);
            let ranges = ranges(tcx, span, &mut out.f);
            if ranges == (None, None) {
                last = None;
                continue;
            }
            if last == Some(ranges) && let Some(run) = out.r.last_mut() {
                run[1] += 1;
                continue;
            }
            last = Some(ranges);
            let (tokens, call) = ranges;
            let (file, start, end) = tokens.unwrap_or((-1, 0, 0));
            let mut run = vec![index as i64, 1, file, start, end];
            if let Some((file, start, end)) = call {
                run.extend([file, start, end]);
            }
            out.r.push(run);
        }
    });
    if let Some(item) = item
        && let Some((symbols, per, own, evaluated)) = crate::guarded(|| crate::symbols::elements(body, item, &mut out.f))
    {
        out.s = symbols;
        out.o = own;
        out.c = evaluated;
        out.v = 1;
        let mut index = 0;
        while index < per.len() {
            let mut end = index + 1;
            while end < per.len() && per[end] == per[index] {
                end += 1;
            }
            if !per[index].is_empty() {
                let mut run = vec![index as u32, (end - index) as u32];
                run.extend(&per[index]);
                out.y.push(run);
            }
            index = end;
        }
    }
    out
}

/// The range of the tokens of a span, and the range of its outermost macro call.
fn ranges(tcx: rustc_middle::ty::TyCtxt<'_>, span: Span, files: &mut Vec<String>) -> (Option<Range>, Option<Range>) {
    let tokens = range(tcx, span, files);
    let mut call = None;
    let mut current = span;
    // A desugaring (`for`, `?`, `async`) is not a macro: its span is the source of the user.
    while current.from_expansion() {
        let data = current.ctxt().outer_expn_data();
        if matches!(data.kind, ExpnKind::Macro(..)) {
            call = Some(data.call_site);
        }
        if data.call_site == current {
            break;
        }
        current = data.call_site;
    }
    let call = call.and_then(|call| range(tcx, call.source_callsite(), files));
    (tokens, call)
}

/// The range of a span in a file of the workspace, in the file as written. `None` for a span
/// with no place, and for a file outside the workspace (the standard library, a registry
/// crate).
pub(crate) fn range(tcx: rustc_middle::ty::TyCtxt<'_>, span: Span, files: &mut Vec<String>) -> Option<Range> {
    if span.is_dummy() {
        return None;
    }
    let source_map = tcx.sess.source_map();
    let file = source_map.lookup_source_file(span.lo());
    if !file.contains(span.hi()) || span.hi() < span.lo() {
        return None;
    }
    let rustc_span::FileName::Real(name) = &file.name else { return None };
    let path = crate::workspace_path(name.local_path()?);
    // An absolute path is outside the workspace.
    if path.is_empty() || std::path::Path::new(&path).is_absolute() {
        return None;
    }
    let start = file.original_relative_byte_pos(span.lo()).0 as i64;
    let end = file.original_relative_byte_pos(span.hi()).0 as i64;
    let index = match files.iter().position(|known| *known == path) {
        Some(index) => index,
        None => {
            files.push(path);
            files.len() - 1
        }
    };
    Some((index as i64, start, end))
}

/// The byte range of the span of a body in the file as written. `None` for a span from a macro
/// expansion and for a file outside the workspace.
pub fn body_range(span: rustc_public::ty::Span) -> Option<(i64, i64)> {
    rustc_middle::ty::tls::with(|tcx| {
        let span = rustc_public::rustc_internal::internal(tcx, span);
        if span.from_expansion() {
            return None;
        }
        range(tcx, span, &mut Vec::new()).map(|(_, start, end)| (start, end))
    })
}
