//! The `main` function of the test harness.
//!
//! For a test build (`--test`), rustc adds a `main` function to the root of the crate. It calls
//! the test runner with the list of the tests: `test::test_main_env_args(&[&test_a, &test_b])`.
//! This function is not a body of the user. In a binary crate, it has the same def path as the
//! `main` function of the user (`k::main`), so the two bodies must not share one record name.
//!
//! The driver finds the function by its origin in the compiler, not by its name:
//!
//! - the session compiles a test crate (`--test`);
//! - the span of the item comes from the AST pass `TestHarness` (user code and user macros
//!   cannot make a span of this pass);
//! - the item is the entry function of the crate.
//!
//! The record of this function has the name `<crate>::{test-harness-main}`. No def path of user
//! code has this text: an identifier has no braces, and each segment that rustc generates has
//! the form `{kind#N}`.
//!
//! The text of the record has the test runner, and not the list of the tests. Each entry of the
//! list is an item with a test attribute, and that item has its own record with the full check.
//! So an added or a removed test shows as a body that exists on one side only, and the list adds
//! no fact. A body that does not have the known form (statements with no effect, and one call
//! with one argument) keeps its full MIR text.

use rustc_public::mir::{AggregateKind, Body, CastKind, PointerCoercion, Rvalue, StatementKind, TerminatorKind};
use rustc_public::ty::{RigidTy, TyKind};
use rustc_public::CrateDef;
use rustc_span::hygiene::{AstPass, ExpnKind};

/// The last segment of the record name. `semilla-normal` has the same text (`HARNESS_MAIN`).
const SEGMENT: &str = "{test-harness-main}";

/// True if the item is the `main` function that rustc generates for a test build.
pub fn is_harness_main(item: rustc_public::DefId) -> bool {
    rustc_middle::ty::tls::with(|tcx| {
        if !tcx.sess.is_test_crate() {
            return false;
        }
        let def_id = rustc_public::rustc_internal::internal(tcx, item);
        let generated = matches!(tcx.def_span(def_id).ctxt().outer_expn_data().kind, ExpnKind::AstPass(AstPass::TestHarness));
        generated && tcx.entry_fn(()).is_some_and(|(entry, _)| entry == def_id)
    })
}

/// The record name of the harness `main` of the local crate.
pub fn record_name() -> String {
    let krate = rustc_middle::ty::tls::with(|tcx| tcx.crate_name(rustc_span::def_id::LOCAL_CRATE).to_string());
    format!("{krate}::{SEGMENT}")
}

/// The record text of the harness `main`: the test runner that it calls. Returns `None` if the
/// body does not have the known form. Then the caller keeps the full MIR text.
///
/// The known form: no parameter, statements that only build the argument (a use, a reference, an
/// array, an unsize cast), and exactly one call, of a known function, with one argument. Such a
/// body can only give its argument to the runner.
pub fn text(body: &Body) -> Option<String> {
    if !body.arg_locals().is_empty() {
        return None;
    }
    let mut runner = None;
    for block in &body.blocks {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Assign(_, rvalue) => match rvalue {
                    Rvalue::Use(..) | Rvalue::Ref(..) | Rvalue::Aggregate(AggregateKind::Array(_), _) => {}
                    Rvalue::Cast(CastKind::PointerCoercion(PointerCoercion::Unsize), ..) => {}
                    _ => return None,
                },
                StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
                _ => return None,
            }
        }
        match &block.terminator.kind {
            TerminatorKind::Call { func, args, .. } => {
                if runner.is_some() || args.len() != 1 {
                    return None;
                }
                let ty = func.ty(body.locals()).ok()?;
                let TyKind::RigidTy(RigidTy::FnDef(def, generics)) = ty.kind() else { return None };
                if !generics.0.is_empty() {
                    return None;
                }
                let (krate, real) = rustc_middle::ty::tls::with(|tcx| crate::reach::origin(rustc_public::rustc_internal::internal(tcx, def.def_id())));
                runner = Some(format!("{} (the crate `{krate}` defines it as `{real}`)", def.name()));
            }
            TerminatorKind::Return | TerminatorKind::Resume | TerminatorKind::Goto { .. } | TerminatorKind::Abort | TerminatorKind::Unreachable => {}
            _ => return None,
        }
    }
    let runner = runner?;
    Some(format!("test harness main (rustc generates it for a test build)\nrunner: {runner}\nargument: the list of the tests of this target (each test has its own record)\n"))
}
