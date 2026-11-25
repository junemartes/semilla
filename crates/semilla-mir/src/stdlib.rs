//! The crates of the standard library, from the data of the compiler (batch 10n, R65.3-1).
//!
//! A crate is a crate of the standard library if the compiler loaded it from the sysroot of the
//! tool chain (the path of its `rlib` or `rmeta` file is in the sysroot). Before batch 10n, the
//! driver had name lists (`"core" | "std" | "alloc" | "proc_macro" | "test"`) in six files. A
//! name list misses a crate of the sysroot (`compiler_builtins`, the copy of `hashbrown` that
//! `std` uses) and can match a crate of the workspace with the same name.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rustc_middle::ty::TyCtxt;
use rustc_span::def_id::{CrateNum, LOCAL_CRATE};

/// The names of the crates that the compiler loaded from the sysroot, and the names of the
/// other crates. A name in the two sets is not a name of the standard library: a crate of the
/// workspace or of a registry has the name too (`hashbrown`).
static NAMES: OnceLock<(HashSet<String>, HashSet<String>)> = OnceLock::new();

/// True if the compiler loaded the crate from the sysroot of the tool chain.
pub fn is_sysroot_crate(tcx: TyCtxt<'_>, krate: CrateNum) -> bool {
    if krate == LOCAL_CRATE {
        return false;
    }
    thread_local! {
        static MEMO: std::cell::RefCell<HashMap<CrateNum, bool>> = std::cell::RefCell::new(HashMap::new());
    }
    if let Some(found) = MEMO.with(|memo| memo.borrow().get(&krate).copied()) {
        return found;
    }
    let root = tcx.sess.opts.sysroot.path().to_path_buf();
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let found = tcx.used_crate_source(krate).paths().any(|path| {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        path.starts_with(&root)
    });
    MEMO.with(|memo| memo.borrow_mut().insert(krate, found));
    found
}

fn names(tcx: TyCtxt<'_>) -> &'static (HashSet<String>, HashSet<String>) {
    NAMES.get_or_init(|| {
        let mut sysroot = HashSet::new();
        let mut other = HashSet::new();
        other.insert(tcx.crate_name(LOCAL_CRATE).to_string());
        for krate in tcx.crates(()) {
            let name = tcx.crate_name(*krate).to_string();
            if is_sysroot_crate(tcx, *krate) {
                sysroot.insert(name);
            } else {
                other.insert(name);
            }
        }
        (sysroot, other)
    })
}

/// True for the name of a crate of the standard library: a crate that the compiler loaded from
/// the sysroot, and no other loaded crate has the name. Use `is_sysroot_crate` where the crate
/// number is known.
pub fn is_std_name(name: &str) -> bool {
    rustc_middle::ty::tls::with_opt(|tcx| match tcx {
        Some(tcx) => {
            let (sysroot, other) = names(tcx);
            sysroot.contains(name) && !other.contains(name)
        }
        // No compiler context: the safe answer is "not the standard library".
        None => false,
    })
}
