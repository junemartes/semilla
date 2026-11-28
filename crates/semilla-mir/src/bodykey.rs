//! [R93.3-1] The key of the record of a body (design 93.3).
//!
//! The driver asks the store of Semilla before it prints a body. Semilla gives the keys of the
//! records that the store has (the file of `SEMILLA_MIR_KNOWN`). For each body at the level
//! N0, the driver makes the key of its record before the print. A known key gives a pointer
//! line, and Semilla puts the stored record in its place. Each other body gets its full record,
//! and the driver gives its key to Semilla in the last line of the file.
//!
//! The key has each input of the record:
//!
//! - the tool: the driver (its fingerprint), the level, the variables of the driver, the
//!   arguments of the compile, and the current folder;
//! - the unit: the hash of each crate of the crate graph (`crate_hash`), the def path hash of
//!   each local definition, and the stable hash of each local type, field type, signature,
//!   impl header, and clause (with no span), and of the bodies of the local `Drop` impls;
//! - the body: the stable hash of its MIR (`StableHash`, as rustc incremental uses it), the
//!   stable hash of the HIR of its owner (the names of the expanded code), its generic
//!   parameters, and the MIR of each local item that it can reach (callees, closures, the
//!   local impls of each trait method that it names, the constants and the statics that it
//!   reads);
//! - the places: the source ranges of its elements and of the symbols that they name (`el`),
//!   its span, its macro call, the lines of its names, and the place (the span of the
//!   definition) of each local item that it can reach. The stable hash of a span is relative
//!   to its item, so a line above the item does not change the MIR hash. [R99.8-1] Each place
//!   is relative to the innermost item of the unit that holds it (`anchors.rs`, design 99.8):
//!   a line above an item does not change the key, and Semilla moves the places of a stored
//!   record by the start of each item in the new tree.
//!
//! A body whose work calls `print::unique` gets no key: its text is never equal to another.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::Hasher as _;

use rustc_data_structures::fingerprint::Fingerprint;
use rustc_data_structures::stable_hash::{StableHash, StableHasher};
use rustc_middle::ty::{self, TyCtxt, TypeVisitableExt as _};
use rustc_span::def_id::DefId;
use sha2::{Digest as _, Sha256};

/// The variable with the path of the file of the known keys.
pub(crate) const KNOWN_ENV: &str = "SEMILLA_MIR_KNOWN";

thread_local! {
    /// [R99.8-1] The table of the items of the unit of this compile, for the last line of the
    /// record file.
    static ANCHOR_TEXT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// [R99.8-1] The JSON text of the table of the items of the unit (one time).
pub(crate) fn take_anchor_text() -> Option<String> {
    ANCHOR_TEXT.with(|text| text.borrow_mut().take())
}

/// The keys of one compile.
pub(crate) struct Keys {
    known: HashSet<String>,
    unit: Fingerprint,
    mir: std::cell::RefCell<HashMap<DefId, Option<Fingerprint>>>,
    mentions: std::cell::RefCell<HashMap<DefId, Vec<DefId>>>,
    /// For each reached item: its def path hash, its place (if a record can name it), and the
    /// state "closure or generic".
    items: std::cell::RefCell<HashMap<DefId, (Fingerprint, bool, bool, std::rc::Rc<str>)>>,
    /// The reach hash of each item, by the state "with the places of the generic items".
    reach: std::cell::RefCell<HashMap<(DefId, bool), Fingerprint>>,
    /// For each trait item: the local items that implement it.
    implementors: HashMap<DefId, Vec<DefId>>,
    /// The crates of the workspace that this crate uses (each one has an interface file): the
    /// walk of a key goes into their MIR.
    workspace: HashSet<rustc_span::def_id::CrateNum>,
    /// [R99.8-1] The items of the unit: a place of a key is relative to its item (design 99.8).
    anchors: crate::anchors::Table,
    /// [R99.8-1] The columns of the start and of the end of each item of `anchors`.
    columns: Vec<Vec<(usize, usize)>>,
}

/// The stable hash of a value.
fn stable<'tcx, T: StableHash>(tcx: TyCtxt<'tcx>, value: &T) -> Fingerprint {
    tcx.with_stable_hashing_context(|mut hcx| {
        let mut hasher = StableHasher::new();
        value.stable_hash(&mut hcx, &mut hasher);
        hasher.finish()
    })
}

/// The bytes of a fingerprint.
fn bytes(fingerprint: Fingerprint) -> [u8; 16] {
    let (one, two) = fingerprint.split();
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&one.as_u64().to_le_bytes());
    out[8..].copy_from_slice(&two.as_u64().to_le_bytes());
    out
}

impl Keys {
    /// The keys of this compile, or `None`: Semilla gives no file of known keys, or the driver
    /// cannot make the hash of the unit.
    pub(crate) fn of(tcx: TyCtxt<'_>, level: &str) -> Option<Keys> {
        let path = std::env::var_os(KNOWN_ENV)?;
        let text = std::fs::read_to_string(path).ok()?;
        let known: HashSet<String> = text.lines().filter(|line| !line.is_empty()).map(str::to_string).collect();
        let (unit, implementors, workspace) = crate::guarded(|| unit_hash(tcx, level))?;
        let (anchors, columns) = crate::guarded(|| anchor_table(tcx)).unwrap_or_default();
        ANCHOR_TEXT.with(|text| *text.borrow_mut() = serde_json::to_string(&anchors).ok());
        Some(Keys { known, unit, mir: Default::default(), mentions: Default::default(), items: Default::default(), reach: Default::default(), implementors, workspace, anchors: crate::anchors::Table::new(anchors), columns })
    }

    /// [R99.8-1] A place of a key: the item that holds it (its identity and the columns of its
    /// start and end) and the offset in the item, or the place itself if no item holds it.
    fn relative(&self, kind: crate::anchors::Kind, file: &str, at: i64) -> String {
        match self.anchors.find(kind, file, at) {
            Some(found) => {
                let anchor = self.anchors.anchor(found);
                let (start, end) = self.columns[found.0][found.1];
                format!("{}:{start}:{end}+{}", anchor.id, at - crate::anchors::Table::start(anchor, kind))
            }
            None => at.to_string(),
        }
    }

    /// [R99.8-1] A line of a key in the file `file` (see [`Keys::relative`]).
    pub(crate) fn relative_line(&self, file: &str, line: i64) -> String {
        self.relative(crate::anchors::Kind::Line, file, line)
    }

    /// [R93.3-1, R99.8-1] The place of a span in a key: for each span of its chain of macro
    /// calls, the file, the start and the end (as written), the line and the column of each, and
    /// the definition of the macro; each offset and line relative to its item (design 99.8).
    fn place(&self, tcx: TyCtxt<'_>, span: rustc_span::Span) -> String {
        use crate::anchors::Kind;
        let map = tcx.sess.source_map();
        let mut out = String::new();
        let mut current = span;
        for _ in 0..16 {
            if current.is_dummy() {
                out.push_str("- ");
                break;
            }
            let lo = map.lookup_byte_offset(current.lo());
            let hi = map.lookup_byte_offset(current.hi());
            let (lo_file, hi_file) = (table_name(&lo.sf), table_name(&hi.sf));
            let (start, end) = (lo.sf.original_relative_byte_pos(current.lo()).0 as i64, hi.sf.original_relative_byte_pos(current.hi()).0 as i64);
            let (first, last) = (line_column(&lo.sf, current.lo()), line_column(&hi.sf, current.hi()));
            out.push_str(&format!(
                "{} {} {} {} {} {} {} {}; ",
                file_name(&lo.sf),
                self.relative(Kind::Byte, &lo_file, start),
                hi.sf.start_pos == lo.sf.start_pos,
                self.relative(Kind::Byte, &hi_file, end),
                self.relative(Kind::Line, &lo_file, first.0),
                first.1,
                self.relative(Kind::Line, &hi_file, last.0),
                last.1
            ));
            if !current.from_expansion() {
                break;
            }
            let data = current.ctxt().outer_expn_data();
            out.push_str(&format!("from {} ", self.place_short(tcx, data.def_site)));
            current = data.call_site;
        }
        out
    }

    /// [R99.8-1] The place of the definition of a macro in a key.
    fn place_short(&self, tcx: TyCtxt<'_>, span: rustc_span::Span) -> String {
        if span.is_dummy() {
            return "-".into();
        }
        let lo = tcx.sess.source_map().lookup_byte_offset(span.lo());
        let name = table_name(&lo.sf);
        let (start, end) = (lo.sf.original_relative_byte_pos(span.lo()).0 as i64, lo.sf.original_relative_byte_pos(span.hi()).0 as i64);
        format!("{} {} {}", file_name(&lo.sf), self.relative(crate::anchors::Kind::Byte, &name, start), self.relative(crate::anchors::Kind::Byte, &name, end))
    }

    /// True if the store has a record with this key.
    pub(crate) fn is_known(&self, key: &str) -> bool {
        self.known.contains(key)
    }

    /// The stable hash of the MIR of a local item, or `None` (no MIR).
    fn mir(&self, tcx: TyCtxt<'_>, def: DefId) -> Option<Fingerprint> {
        if let Some(found) = self.mir.borrow().get(&def) {
            return *found;
        }
        let found = crate::guarded(|| if def.is_local() { mir_hash(tcx, def) } else { extern_body(tcx, def, &self.workspace).map(|body| stable(tcx, body)) }).flatten();
        self.mir.borrow_mut().insert(def, found);
        found
    }

    /// The local items that the MIR of an item names: the functions, the closures, and the
    /// coroutines in its types and constants, and the local implementors of each trait item.
    fn mentions(&self, tcx: TyCtxt<'_>, def: DefId) -> Vec<DefId> {
        if let Some(found) = self.mentions.borrow().get(&def) {
            return found.clone();
        }
        let found = crate::guarded(|| mentions_of(tcx, def, &self.implementors, &self.workspace)).unwrap_or_default();
        self.mentions.borrow_mut().insert(def, found.clone());
        found
    }

    /// [R93.3-1] The places of a body with no print (see [`places_key`]), with the place of each
    /// callee that the compiler resolves (from the mentions).
    pub(crate) fn places(&self, tcx: TyCtxt<'_>, def: DefId) -> Option<String> {
        let mentions = self.mentions(tcx, def);
        places_key(self, tcx, def, &mentions, &|item| self.item(tcx, item).3)
    }

    /// [R93.3-1] The hash of the items that `def` reaches (itself too), to any depth: for each
    /// strongly connected part of the graph of the mentions, the def path hash, the MIR hash,
    /// and the place (a closure; with `generic_places`, a generic item) of each member, and the
    /// hash of each part that it reaches. Two items with equal reach have equal hashes; a change
    /// of a reached item changes the hash. One walk in a compile (Tarjan, with no recursion).
    fn reach_hash(&self, tcx: TyCtxt<'_>, def: DefId, generic_places: bool) -> Fingerprint {
        if let Some(found) = self.reach.borrow().get(&(def, generic_places)) {
            return *found;
        }
        struct Frame {
            item: DefId,
            next: Vec<DefId>,
            at: usize,
        }
        let mut index: HashMap<DefId, (usize, usize)> = HashMap::new();
        let mut on_stack: HashSet<DefId> = HashSet::new();
        let mut stack: Vec<DefId> = Vec::new();
        let mut frames: Vec<Frame> = Vec::new();
        let mut counter = 0usize;
        let start = |item: DefId, index: &mut HashMap<DefId, (usize, usize)>, counter: &mut usize, stack: &mut Vec<DefId>, on_stack: &mut HashSet<DefId>| -> Frame {
            index.insert(item, (*counter, *counter));
            *counter += 1;
            stack.push(item);
            on_stack.insert(item);
            Frame { item, next: self.mentions(tcx, item), at: 0 }
        };
        frames.push(start(def, &mut index, &mut counter, &mut stack, &mut on_stack));
        while let Some(frame) = frames.last_mut() {
            if frame.at < frame.next.len() {
                let next = frame.next[frame.at];
                frame.at += 1;
                if self.reach.borrow().contains_key(&(next, generic_places)) {
                    continue;
                }
                match index.get(&next).copied() {
                    None => {
                        let started = start(next, &mut index, &mut counter, &mut stack, &mut on_stack);
                        frames.push(started);
                    }
                    Some((order, _)) if on_stack.contains(&next) => {
                        let item = frame.item;
                        let entry = index.get_mut(&item).expect("an open item");
                        entry.1 = entry.1.min(order);
                    }
                    Some(_) => {}
                }
                continue;
            }
            let frame = frames.pop().expect("a frame");
            let (order, low) = index[&frame.item];
            if let Some(parent) = frames.last() {
                let parent_entry = index.get_mut(&parent.item).expect("an open item");
                parent_entry.1 = parent_entry.1.min(low);
            }
            if order != low {
                continue;
            }
            // The item is the root of a part: the members are on the stack above it.
            let mut members = Vec::new();
            while let Some(member) = stack.pop() {
                on_stack.remove(&member);
                members.push(member);
                if member == frame.item {
                    break;
                }
            }
            let mut own: Vec<(Fingerprint, Fingerprint, std::rc::Rc<str>)> = Vec::new();
            let mut outside: BTreeSet<Fingerprint> = BTreeSet::new();
            for member in &members {
                let (path, closure, generic, place) = self.item(tcx, *member);
                let named = closure || (generic_places && generic);
                own.push((path, self.mir(tcx, *member).unwrap_or(Fingerprint::ZERO), if named { place } else { std::rc::Rc::from("") }));
                for next in self.mentions(tcx, *member) {
                    if let Some(found) = self.reach.borrow().get(&(next, generic_places)) {
                        outside.insert(*found);
                    }
                }
            }
            own.sort();
            let mut hasher = StableHasher::new();
            for (path, hash, place) in &own {
                hasher.write(&bytes(*path));
                hasher.write(&bytes(*hash));
                hasher.write_u64(place.len() as u64);
                hasher.write(place.as_bytes());
            }
            for found in &outside {
                hasher.write(&bytes(*found));
            }
            let hash: Fingerprint = hasher.finish();
            let mut reach = self.reach.borrow_mut();
            for member in members {
                reach.insert((member, generic_places), hash);
            }
        }
        self.reach.borrow().get(&(def, generic_places)).copied().unwrap_or(Fingerprint::ZERO)
    }

    /// The facts of a reached item (see [`Keys::items`]), one time in a compile.
    fn item(&self, tcx: TyCtxt<'_>, def: DefId) -> (Fingerprint, bool, bool, std::rc::Rc<str>) {
        if let Some(found) = self.items.borrow().get(&def) {
            return found.clone();
        }
        // [R99.8-1] The head of the definition (the place of a closure name), and for a local
        // definition its full range (the symbol table of a record, `symbols.rs`).
        let mut place = self.place(tcx, tcx.def_span(def));
        if let Some(local) = def.as_local() {
            place.push_str("full ");
            place.push_str(&self.place(tcx, tcx.source_span(local).source_callsite()));
        }
        let found = (tcx.def_path_hash(def).0, tcx.is_closure_like(def), tcx.generics_of(def).requires_monomorphization(tcx), std::rc::Rc::from(place));
        self.items.borrow_mut().insert(def, found.clone());
        found
    }

    /// The key of the record of a body: `None` if a part has no stable hash.
    pub(crate) fn key(&self, tcx: TyCtxt<'_>, def: DefId, parts: &[&str]) -> Option<String> {
        let own = self.mir(tcx, def)?;
        // Each local item that the body can reach, with the hash of its MIR.
        // The place of each one in its file: the stable hash of a span is relative to its item,
        // and a record names the places of other items (the name of a closure type, the ranges
        // of the form of an instance).
        // [R93.3-1] A record names the place of a reached closure (its type name) and, for a
        // body with no generic parameter, the places of the reached generic items (the forms
        // of their instances); each other reached item adds only its MIR.
        let generic_body = tcx.generics_of(def).requires_monomorphization(tcx);
        let reach = self.reach_hash(tcx, def, !generic_body);
        let owner = def.as_local().map(|local| tcx.local_def_id_to_hir_id(local).owner);
        let hir = match owner {
            Some(owner) => tcx.hir_owner_nodes(owner).opt_hash?,
            None => return None,
        };
        let mut hasher = Sha256::new();
        hasher.update(bytes(self.unit));
        hasher.update(bytes(own));
        hasher.update(bytes(hir));
        hasher.update(bytes(reach));
        for part in parts {
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        let digest = hasher.finalize();
        Some(digest[..16].iter().map(|byte| format!("{byte:02x}")).collect())
    }
}

/// The stable hash of the MIR of an item (`StableHash`, as rustc incremental uses it).
pub(crate) fn mir_hash(tcx: TyCtxt<'_>, def: DefId) -> Option<Fingerprint> {
    use rustc_hir::def::DefKind;
    // Each query asks only about an item with a body of its own (a constant of a trait with no
    // default has none, and a query of its body stops the compile).
    tcx.hir_maybe_body_owned_by(def.as_local()?)?;
    // A trivial constant has no MIR: its value is the part of the key.
    if matches!(tcx.def_kind(def), DefKind::Const { .. } | DefKind::AssocConst { .. }) && tcx.is_trivial_const(def) {
        let mut hasher = StableHasher::new();
        hasher.write(format!("{:?}", tcx.trivial_const(def)).as_bytes());
        return Some(hasher.finish());
    }
    let body = body_of(tcx, def)?;
    // The promoted constants of a body are bodies of their own: their MIR is a part of the
    // hash of the body.
    let mut hasher = StableHasher::new();
    hasher.write(&bytes(stable(tcx, body)));
    for promoted in tcx.promoted_mir(def).iter() {
        hasher.write(&bytes(stable(tcx, promoted)));
    }
    Some(hasher.finish())
}

/// The MIR of a local item with a body: the MIR of the evaluation of a constant or a static
/// initializer, the optimized MIR of each other body. Each query asks only for MIR that exists:
/// a query for a missing body stops the compile.
fn body_of<'tcx>(tcx: TyCtxt<'tcx>, def: DefId) -> Option<&'tcx rustc_middle::mir::Body<'tcx>> {
    use rustc_hir::def::DefKind;
    let local = def.as_local()?;
    tcx.hir_maybe_body_owned_by(local)?;
    match tcx.def_kind(def) {
        DefKind::Const { .. } | DefKind::AssocConst { .. } if tcx.is_trivial_const(def) => None,
        DefKind::Const { .. } | DefKind::AssocConst { .. } | DefKind::AnonConst | DefKind::Static { .. } => Some(tcx.mir_for_ctfe(def)),
        _ if tcx.is_mir_available(def) => Some(tcx.optimized_mir(def)),
        _ => None,
    }
}

/// [R93.3-1] The MIR of an item of a crate of the workspace that this crate uses, if its
/// metadata has it (a generic or an inline function): the record of a body reads it through
/// an instance or an effect summary.
fn extern_body<'tcx>(tcx: TyCtxt<'tcx>, def: DefId, workspace: &HashSet<rustc_span::def_id::CrateNum>) -> Option<&'tcx rustc_middle::mir::Body<'tcx>> {
    use rustc_hir::def::DefKind;
    if def.is_local() || !workspace.contains(&def.krate) || !matches!(tcx.def_kind(def), DefKind::Fn | DefKind::AssocFn | DefKind::Closure) || !tcx.is_mir_available(def) {
        return None;
    }
    Some(tcx.optimized_mir(def))
}

/// The name of a source file, one text for each file in a compile.
fn file_name(file: &rustc_span::SourceFile) -> std::rc::Rc<str> {
    thread_local! {
        static NAMES: std::cell::RefCell<HashMap<u32, std::rc::Rc<str>>> = std::cell::RefCell::new(HashMap::new());
    }
    NAMES.with(|names| names.borrow_mut().entry(file.start_pos.0).or_insert_with(|| std::rc::Rc::from(format!("{:?}", file.name))).clone())
}

/// [R99.8-1] The line (from 1) of a position, and its column in bytes from the start of the
/// line: a cheaper search than the column in characters.
fn line_column(file: &rustc_span::SourceFile, pos: rustc_span::BytePos) -> (i64, u32) {
    let line = file.lookup_line(file.relative_position(pos)).map_or(0, |line| line as i64 + 1);
    (line, (pos - file.line_begin_pos(pos)).0)
}

/// [R99.8-1] The name of a source file in the table of the items: the path from the root of
/// the workspace (as in the places of a record), or empty for a file that is not real.
fn table_name(file: &rustc_span::SourceFile) -> String {
    match &file.name {
        rustc_span::FileName::Real(name) => name.local_path().map(crate::workspace_path).unwrap_or_default(),
        _ => String::new(),
    }
}

/// [R99.8-1] The table of the items of the unit (design 99.8): each local definition with a
/// place in a file of the workspace (not a generic parameter or a `use`), with its full range
/// (at the macro call for a definition from an expansion), and the columns of its start and end.
fn anchor_table(tcx: TyCtxt<'_>) -> (Vec<crate::anchors::FileAnchors>, Vec<Vec<(usize, usize)>>) {
    use rustc_hir::def::DefKind;
    let map = tcx.sess.source_map();
    let mut files: Vec<crate::anchors::FileAnchors> = Vec::new();
    let mut columns: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for local in tcx.hir_crate_items(()).definitions() {
        if matches!(tcx.def_kind(local), DefKind::TyParam | DefKind::LifetimeParam | DefKind::ConstParam | DefKind::Use | DefKind::ExternCrate | DefKind::ForeignMod | DefKind::GlobalAsm) {
            continue;
        }
        let span = tcx.source_span(local).source_callsite();
        if span.is_dummy() || span.hi() < span.lo() {
            continue;
        }
        let lo = map.lookup_byte_offset(span.lo());
        if !lo.sf.contains(span.hi()) {
            continue;
        }
        let rustc_span::FileName::Real(name) = &lo.sf.name else { continue };
        let Some(local_path) = name.local_path() else { continue };
        let path = crate::workspace_path(local_path);
        if path.is_empty() || std::path::Path::new(&path).is_absolute() {
            continue;
        }
        let at = *index.entry(path.clone()).or_insert_with(|| {
            let mut names = vec![path.clone()];
            let given = local_path.display().to_string();
            if given != path {
                names.push(given);
            }
            files.push(crate::anchors::FileAnchors { names, anchors: Vec::new() });
            columns.push(Vec::new());
            files.len() - 1
        });
        let (first, last) = (map.lookup_char_pos(span.lo()), map.lookup_char_pos(span.hi()));
        let hash = bytes(tcx.def_path_hash(local.to_def_id()).0);
        files[at].anchors.push(crate::anchors::Anchor {
            id: hash.iter().map(|byte| format!("{byte:02x}")).collect(),
            lo: lo.sf.original_relative_byte_pos(span.lo()).0 as i64,
            hi: lo.sf.original_relative_byte_pos(span.hi()).0 as i64,
            line_lo: first.line as i64,
            line_hi: last.line as i64,
        });
        columns[at].push((first.col.0, last.col.0));
    }
    (files, columns)
}

/// [R93.3-1] The places that the record of a body reads, with no print (the part of the key in
/// place of the element ranges, the span, and the macro call): the place of each span of its
/// MIR (with its macro calls and definitions), the line of its span, and the place of each
/// definition that its MIR names (with the variants and the fields of a type, and the callee of
/// a resolved call).
fn places_key<'tcx>(keys: &Keys, tcx: TyCtxt<'tcx>, def: DefId, extra: &[DefId], place: &dyn Fn(DefId) -> std::rc::Rc<str>) -> Option<String> {
    let body = body_of(tcx, def)?;
    let map = tcx.sess.source_map();
    let mut spans: Vec<rustc_span::Span> = vec![body.span];
    for block in body.basic_blocks.iter() {
        spans.extend(block.statements.iter().map(|statement| statement.source_info.span));
        if let Some(terminator) = &block.terminator {
            spans.push(terminator.source_info.span);
        }
    }
    spans.extend(body.local_decls.iter().map(|decl| decl.source_info.span));
    spans.extend(body.var_debug_info.iter().map(|info| info.source_info.span));
    spans.extend(body.user_type_annotations.iter().map(|annotation| annotation.span));
    let mut places: BTreeSet<String> = spans.into_iter().map(|span| keys.place(tcx, span)).collect();
    // [R99.8-1] The lines of the span of the record, in the file of the span as the record
    // names it.
    let lo = map.lookup_char_pos(body.span.lo());
    let hi = map.lookup_char_pos(body.span.hi());
    let file = table_name(&lo.file);
    places.insert(format!("lines {} {} {}", keys.relative_line(&file, lo.line as i64), lo.col.0, keys.relative_line(&file, hi.line as i64)));
    // The definitions that the MIR names (the resolved callees come from the mentions of the
    // key, [`Keys::places`]).
    let mut named: HashSet<DefId> = HashSet::new();
    let add_ty = |ty: ty::Ty<'tcx>, named: &mut HashSet<DefId>| {
        for arg in ty.walk() {
            let Some(ty) = arg.as_type() else { continue };
            match ty.kind() {
                ty::Adt(adt, _) => {
                    named.insert(adt.did());
                    // The drop glue of the type runs the `Drop` impl of the type and of the types
                    // of its fields, to any depth: the record names the place of each impl and of
                    // its method.
                    let mut todo = vec![adt.did()];
                    let mut seen: HashSet<DefId> = HashSet::new();
                    while let Some(next) = todo.pop() {
                        if !seen.insert(next) || seen.len() > 4096 {
                            continue;
                        }
                        if let Some(destructor) = tcx.adt_destructor(next) {
                            named.insert(destructor.did);
                            if let Some(parent) = tcx.opt_parent(destructor.did) {
                                named.insert(parent);
                            }
                        }
                        for variant in tcx.adt_def(next).variants() {
                            for field in &variant.fields {
                                for inner in tcx.type_of(field.did).skip_binder().walk() {
                                    if let Some(inner) = inner.as_type()
                                        && let ty::Adt(inner, _) = inner.kind()
                                    {
                                        todo.push(inner.did());
                                    }
                                }
                            }
                        }
                    }
                    for variant in adt.variants() {
                        named.insert(variant.def_id);
                        named.extend(variant.fields.iter().map(|field| field.did));
                    }
                }
                ty::FnDef(callee, _) => {
                    named.insert(*callee);
                }
                ty::Closure(item, _) | ty::Coroutine(item, _) | ty::CoroutineClosure(item, _) | ty::Foreign(item) => {
                    named.insert(*item);
                }
                ty::Dynamic(predicates, ..) => named.extend(predicates.principal_def_id()),
                _ => {}
            }
        }
    };
    for decl in body.local_decls.iter() {
        add_ty(decl.ty, &mut named);
    }
    for constant in body.required_consts() {
        add_ty(constant.const_.ty(), &mut named);
        if let rustc_middle::mir::Const::Unevaluated(unevaluated, _) = constant.const_ {
            named.insert(unevaluated.def);
        }
    }
    for item in named.into_iter().chain(extra.iter().copied()) {
        places.insert(format!("def {:?} {}", tcx.def_path_hash(item).0, place(item)));
    }
    Some(places.into_iter().collect::<Vec<_>>().join("\n"))
}

/// The local items that the MIR of `def` names (see [`Keys::mentions`]): each function, closure,
/// and coroutine in a type of a local or a constant (and the callee of a resolved call), each
/// constant (`Unevaluated`), and each static that a constant points to. The walk goes into the
/// MIR of the crates of the workspace that this crate uses.
fn mentions_of(tcx: TyCtxt<'_>, def: DefId, implementors: &HashMap<DefId, Vec<DefId>>, workspace: &HashSet<rustc_span::def_id::CrateNum>) -> Vec<DefId> {
    use rustc_middle::mir::visit::Visitor;
    struct Walk<'a, 'tcx> {
        tcx: TyCtxt<'tcx>,
        implementors: &'a HashMap<DefId, Vec<DefId>>,
        workspace: &'a HashSet<rustc_span::def_id::CrateNum>,
        typing: ty::TypingEnv<'tcx>,
        found: HashSet<DefId>,
    }
    impl<'tcx> Walk<'_, 'tcx> {
        fn add_ty(&mut self, ty: ty::Ty<'tcx>) {
            for arg in ty.walk() {
                let Some(ty) = arg.as_type() else { continue };
                let named = match ty.kind() {
                    ty::FnDef(def, _) | ty::Closure(def, _) | ty::Coroutine(def, _) | ty::CoroutineClosure(def, _) => Some(*def),
                    _ => None,
                };
                if let Some(named) = named {
                    if named.is_local() || self.workspace.contains(&named.krate) {
                        self.found.insert(named);
                    }
                    if let Some(local) = self.implementors.get(&named) {
                        self.found.extend(local.iter().copied());
                    }
                }
                // The callee of a call that the compiler resolves (a method of an impl in this
                // crate or in a crate of the workspace).
                if let ty::FnDef(callee, args) = ty.kind()
                    && self.tcx.trait_of_assoc(*callee).is_some()
                    && !args.has_param()
                    && let Ok(Some(instance)) = ty::Instance::try_resolve(self.tcx, self.typing, *callee, args.skip_binder())
                {
                    let resolved = instance.def_id();
                    if resolved.is_local() || self.workspace.contains(&resolved.krate) {
                        self.found.insert(resolved);
                    }
                }
            }
        }
    }
    impl Walk<'_, '_> {
        /// The local items of an allocation of a constant: a static, a function, the local
        /// implementors of a vtable, and the allocations that its pointers name (to a depth).
        fn alloc(&mut self, alloc: rustc_middle::mir::interpret::AllocId, depth: usize) {
            use rustc_middle::mir::interpret::GlobalAlloc;
            match self.tcx.try_get_global_alloc(alloc) {
                Some(GlobalAlloc::Static(def)) if def.is_local() => {
                    self.found.insert(def);
                }
                Some(GlobalAlloc::Function { instance, .. }) => {
                    let def = instance.def_id();
                    if def.is_local() {
                        self.found.insert(def);
                    }
                    if let Some(local) = self.implementors.get(&def) {
                        self.found.extend(local.iter().copied());
                    }
                }
                Some(GlobalAlloc::VTable(_, predicates)) => {
                    let Some(principal) = predicates.principal_def_id() else { return };
                    for item in self.tcx.associated_item_def_ids(principal) {
                        if let Some(local) = self.implementors.get(item) {
                            self.found.extend(local.iter().copied());
                        }
                    }
                }
                Some(GlobalAlloc::Memory(memory)) if depth < 8 => {
                    let pointers: Vec<_> = memory.inner().provenance().ptrs().iter().map(|(_, provenance)| provenance.alloc_id()).collect();
                    for next in pointers {
                        self.alloc(next, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }
    impl<'tcx> Visitor<'tcx> for Walk<'_, 'tcx> {
        fn visit_const_operand(&mut self, constant: &rustc_middle::mir::ConstOperand<'tcx>, _location: rustc_middle::mir::Location) {
            self.add_ty(constant.const_.ty());
            match constant.const_ {
                rustc_middle::mir::Const::Unevaluated(unevaluated, _) => {
                    if unevaluated.def.is_local() {
                        self.found.insert(unevaluated.def);
                    }
                    // A constant of a trait: the local impls give its value.
                    if let Some(local) = self.implementors.get(&unevaluated.def) {
                        self.found.extend(local.iter().copied());
                    }
                }
                rustc_middle::mir::Const::Val(rustc_middle::mir::ConstValue::Scalar(rustc_middle::mir::interpret::Scalar::Ptr(pointer, _)), _) => self.alloc(pointer.provenance.alloc_id(), 0),
                rustc_middle::mir::Const::Val(rustc_middle::mir::ConstValue::Indirect { alloc_id, .. }, _) => self.alloc(alloc_id, 0),
                rustc_middle::mir::Const::Val(rustc_middle::mir::ConstValue::Slice { alloc_id, .. }, _) => self.alloc(alloc_id, 0),
                _ => {}
            }
        }
        fn visit_local_decl(&mut self, _local: rustc_middle::mir::Local, decl: &rustc_middle::mir::LocalDecl<'tcx>) {
            self.add_ty(decl.ty);
        }
    }
    let Some(body) = body_of(tcx, def).or_else(|| extern_body(tcx, def, workspace)) else { return Vec::new() };
    let mut walk = Walk { tcx, implementors, workspace, typing: ty::TypingEnv::post_analysis(tcx, def), found: HashSet::new() };
    walk.visit_body(body);
    for constant in body.required_consts() {
        walk.visit_const_operand(constant, rustc_middle::mir::Location::START);
    }
    if def.is_local() {
        for promoted in tcx.promoted_mir(def).iter() {
            walk.visit_body(promoted);
        }
    }
    let mut found: Vec<DefId> = walk.found.into_iter().filter(|named| *named != def).collect();
    found.sort_by_key(|def| tcx.def_path_hash(*def));
    found
}

/// The hash of the unit (see the module text), and the local implementors of each trait item.
fn unit_hash(tcx: TyCtxt<'_>, level: &str) -> (Fingerprint, HashMap<DefId, Vec<DefId>>, HashSet<rustc_span::def_id::CrateNum>) {
    let mut hasher = StableHasher::new();
    let text = |hasher: &mut StableHasher, part: &str| {
        hasher.write_u64(part.len() as u64);
        hasher.write(part.as_bytes());
    };
    // The tool.
    text(&mut hasher, level);
    for name in ["SEMILLA_MIR_RUN", "SEMILLA_MIR_FORMS", "SEMILLA_TEST_RULE_OFF"] {
        text(&mut hasher, &format!("{name}={:?}", std::env::var(name).ok()));
    }
    for arg in std::env::args() {
        text(&mut hasher, &arg);
    }
    text(&mut hasher, &crate::current_folder().to_string_lossy());
    // The crate graph: a crate of the workspace gives its interface (its file next to its
    // metadata, written by its own compile, with its `crate_hash`); each other crate gives its
    // `crate_hash` (a crate of the registry or of the tool chain changes only with the lock
    // file or the tool chain).
    let mut workspace = HashSet::new();
    for krate in tcx.crates(()).iter() {
        match read_interface(tcx, *krate) {
            Some(interface) => {
                workspace.insert(*krate);
                text(&mut hasher, &format!("{} interface {interface}", tcx.crate_name(*krate)));
            }
            None => text(&mut hasher, &format!("{} {}", tcx.crate_name(*krate), tcx.crate_hash(*krate))),
        }
    }
    let (local, implementors) = local_part(tcx);
    hasher.write(&bytes(local));
    (hasher.finish(), implementors, workspace)
}

/// [R93.3-1] The format line of an interface file.
const INTERFACE_FORMAT: &str = "semilla-interface-1";

/// [R93.3-1] The interface of a crate of the workspace that this crate uses, from its file, or
/// `None`: no file, or a file of another compile (the digest of the metadata file is other).
fn read_interface(tcx: TyCtxt<'_>, krate: rustc_span::def_id::CrateNum) -> Option<String> {
    let source = tcx.used_crate_source(krate);
    let path = source.rmeta.as_ref().or(source.rlib.as_ref())?;
    let text = std::fs::read_to_string(path.with_extension("semilla-interface")).ok()?;
    let mut words = text.split_whitespace();
    if words.next()? != INTERFACE_FORMAT || words.next()? != file_state(path)? {
        return None;
    }
    words.next().map(str::to_string)
}

/// The digest of the content of a file (cargo sets the time of a metadata file after the
/// compile, so the time does not tell the compile).
fn file_state(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(digest[..16].iter().map(|byte| format!("{byte:02x}")).collect())
}

/// [R93.3-1] The interface of this crate: the hash of its local part (the parts of its
/// definitions that a record of another crate reads, 93.3) and of the MIR of the evaluation of
/// each constant, static, and `const fn`. A crate that uses it reads it in place of its
/// `crate_hash`; the MIR of its generic and inline functions is in the walk of each key.
pub(crate) fn interface_hash(tcx: TyCtxt<'_>) -> Option<String> {
    use rustc_hir::def::DefKind;
    let hash = crate::guarded(|| {
        let (local, _) = local_part(tcx);
        let mut hasher = StableHasher::new();
        hasher.write(&bytes(local));
        let mut values: Vec<(Fingerprint, Fingerprint)> = Vec::new();
        for item in tcx.iter_local_def_id() {
            let def = item.to_def_id();
            let evaluated = matches!(tcx.def_kind(def), DefKind::Const { .. } | DefKind::AssocConst { .. } | DefKind::Static { .. }) || (matches!(tcx.def_kind(def), DefKind::Fn | DefKind::AssocFn) && tcx.is_const_fn(def));
            if evaluated && let Some(hash) = mir_hash(tcx, def) {
                values.push((tcx.def_path_hash(def).0, hash));
            }
        }
        values.sort();
        for (path, hash) in values {
            hasher.write(&bytes(path));
            hasher.write(&bytes(hash));
        }
        hasher.finish::<Fingerprint>()
    })?;
    let (one, two) = hash.split();
    Some(format!("{:016x}{:016x}", one.as_u64(), two.as_u64()))
}

/// [R93.3-1] Writes the interface file of a library next to its metadata file, after the
/// compile: the format, the digest of the metadata file (a later compile that writes the
/// metadata again and no interface makes the file stale), and the hash. An error writes no
/// file (the user of the crate takes `crate_hash`).
pub(crate) fn write_interface(args: &[String], hash: &str) {
    let value = |flag: &str| args.iter().position(|arg| arg == flag).and_then(|at| args.get(at + 1)).cloned();
    let extra = args.iter().enumerate().find_map(|(at, arg)| {
        let flag = if arg == "-C" { args.get(at + 1)? } else { arg.strip_prefix("-C")? };
        flag.strip_prefix("extra-filename=").map(str::to_string)
    });
    let (Some(folder), Some(name), Some(extra)) = (value("--out-dir"), value("--crate-name"), extra) else { return };
    if !args.windows(2).any(|pair| pair[0] == "--crate-type" && matches!(pair[1].as_str(), "lib" | "rlib")) {
        return;
    }
    let folder = std::path::Path::new(&folder);
    let interface = folder.join(format!("lib{name}{extra}.semilla-interface"));
    let _ = std::fs::remove_file(&interface);
    let Some(state) = file_state(&folder.join(format!("lib{name}{extra}.rmeta"))) else { return };
    let _ = std::fs::write(interface, format!("{INTERFACE_FORMAT} {state} {hash}\n"));
}

/// The local part of the unit hash: for each local definition, its def path hash and the parts
/// that a record of another item reads; and the local implementors of each trait item.
fn local_part(tcx: TyCtxt<'_>) -> (Fingerprint, HashMap<DefId, Vec<DefId>>) {
    // One time in a compile (the keys and the interface read it).
    thread_local! {
        static PART: std::cell::RefCell<Option<(Fingerprint, HashMap<DefId, Vec<DefId>>)>> = const { std::cell::RefCell::new(None) };
    }
    if let Some(found) = PART.with(|part| part.borrow().clone()) {
        return found;
    }
    let found = local_part_now(tcx);
    PART.with(|part| *part.borrow_mut() = Some(found.clone()));
    found
}

fn local_part_now(tcx: TyCtxt<'_>) -> (Fingerprint, HashMap<DefId, Vec<DefId>>) {
    use rustc_hir::def::DefKind;
    let mut hasher = StableHasher::new();
    // The local definitions, and the parts of each one that a record of another item reads.
    let mut parts: Vec<(Fingerprint, Fingerprint)> = Vec::new();
    let mut implementors: HashMap<DefId, Vec<DefId>> = HashMap::new();
    let drop_trait = tcx.lang_items().drop_trait();
    for local in tcx.iter_local_def_id() {
        let def = local.to_def_id();
        let kind = tcx.def_kind(def);
        let mut item = StableHasher::new();
        item.write(format!("{kind:?}").as_bytes());
        let mut add = |hash: Fingerprint| item.write(&bytes(hash));
        match kind {
            DefKind::Struct | DefKind::Enum | DefKind::Union => {
                let adt = tcx.adt_def(def);
                add(stable(tcx, &adt));
                for variant in adt.variants() {
                    for field in &variant.fields {
                        add(stable(tcx, &tcx.type_of(field.did).skip_binder()));
                    }
                }
            }
            DefKind::TyAlias => add(stable(tcx, &tcx.type_of(def).skip_binder())),
            DefKind::AssocTy if tcx.defaultness(def).has_value() => add(stable(tcx, &tcx.type_of(def).skip_binder())),
            DefKind::Fn | DefKind::AssocFn => add(stable(tcx, &tcx.fn_sig(def).skip_binder())),
            // The value of a constant or a static is a part of the key of each body that names
            // it (`mentions_of`), not of the unit: a test descriptor (a static) has the line of
            // its test.
            DefKind::Const { .. } | DefKind::AssocConst { .. } | DefKind::Static { .. } => add(stable(tcx, &tcx.type_of(def).skip_binder())),
            DefKind::Impl { of_trait } => {
                add(stable(tcx, &tcx.type_of(def).skip_binder()));
                if of_trait {
                    let header = tcx.impl_trait_header(def);
                    add(stable(tcx, &header.trait_ref.skip_binder()));
                    let is_drop = Some(header.trait_ref.skip_binder().def_id) == drop_trait;
                    for impl_item in tcx.associated_item_def_ids(def) {
                        let Some(trait_item) = tcx.associated_item(*impl_item).trait_item_def_id() else { continue };
                        implementors.entry(trait_item).or_default().push(*impl_item);
                        // The drop glue of a type runs the body of its `Drop` impl.
                        if is_drop && let Some(hash) = crate::guarded(|| mir_hash(tcx, *impl_item)).flatten() {
                            add(hash);
                        }
                    }
                }
            }
            _ => {}
        }
        if matches!(kind, DefKind::Struct | DefKind::Enum | DefKind::Union | DefKind::Impl { .. } | DefKind::Trait | DefKind::Fn | DefKind::AssocFn) {
            // The clauses, with no span.
            for (clause, _) in tcx.clauses_of(def).clauses {
                add(stable(tcx, clause));
            }
        }
        parts.push((tcx.def_path_hash(def).0, item.finish()));
    }
    parts.sort();
    for (path, hash) in parts {
        hasher.write(&bytes(path));
        hasher.write(&bytes(hash));
    }
    (hasher.finish(), implementors)
}

