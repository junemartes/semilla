//! The cover rule for the set of conditions of one proof (batch 5t, part B).
//!
//! `cfgcond` decides the state of one condition at one place. A proof can use more than one
//! condition. Then each condition can be covered, and the proof can still be false in a
//! build that Semilla did not check: the checked units have `fast` and `slow` on, or the two
//! off, and the build with only `fast` gives a different result.
//!
//! This module finds the conditions of each proof with the rule itself:
//!
//! 1. The plain run: the rule is off for each covered condition.
//! 2. The strict run: the rule is on for each condition (the rule before batch 5s). A body
//!    with the same result in the two runs uses no covered condition. Its result stands.
//! 3. For each atom of a covered condition, one run with the rule on for the conditions with
//!    this atom. `A` is the set of the atoms that change the result of the body.
//! 4. One run with the rule on for each condition, but not for a condition with each atom in
//!    `A` at a place in a file of the crate of the body. The result must be the plain
//!    result. Then the proof uses only conditions that the unit of the body evaluates, and
//!    the values of the atoms of `A` in this unit give the value of each of them.
//! 5. Each valuation of `A` that can occur must be the valuation of a checked unit that
//!    compiles the body, in the tree before the step and in the tree after it. The two units
//!    must be units of one checked configuration.
//!
//! If one of the steps fails, the body gets the result of the strict run.
//!
//! A valuation cannot occur in these two cases only:
//!
//! - The feature table of the package excludes it: with `fa = ["fb"]`, cargo sets `fb` for
//!   each build with `fa`.
//! - An attribute `#[cfg(P)]` of the body, or of an item around the body in the same file, is
//!   false: the body is not in the build. This cause applies only if the item has the full
//!   line of the body: a line can have the code of two items.
//!
//! A body can have twins under `cfg`: two definitions with one name at two places. A unit
//! compiles one of them. The atoms of the attributes select the twin, and they are possibly
//! not in `A`. So the check uses the place of the body in each unit: a valuation of `A` and
//! of the atoms of the attributes selects one place in each tree, and one checked
//! configuration must have a unit with the body at this place in each tree, with the values
//! of `A`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use semilla_syntax::Workspace;

use crate::cfgcond::{self, Mode, Pred};
use crate::{BodyComparison, BodyStatus, ConfigTruth, Selection, Unit};

/// The start of the gap text in the `config` list of a body.
pub(crate) const GAP: &str = "not checked together: ";

/// A proof with more atoms than this limit keeps the rule on.
const LIMIT: usize = 6;

/// One body of one tree: its place and its units.
#[derive(Debug, Default, Clone)]
pub(crate) struct Seen {
    /// Each place of the body, with its units: the index of the checked configuration, and
    /// the name of the unit. Two twins of a body under `cfg` are at two places.
    places: BTreeMap<Option<(String, usize)>, BTreeSet<(usize, String)>>,
}

/// The bodies of the two trees, by their record names.
pub(crate) type Bodies = [BTreeMap<String, Seen>; 2];

/// The rule of the build configuration, with a list of the units of each body.
pub(crate) struct Recorder<'a> {
    pub(crate) opaque: crate::opaque::Opaque<'a>,
    bodies: RefCell<Bodies>,
}

impl<'a> Recorder<'a> {
    pub(crate) fn new(opaque: crate::opaque::Opaque<'a>) -> Recorder<'a> {
        Recorder { opaque, bodies: RefCell::default() }
    }

    pub(crate) fn bodies(self) -> Bodies {
        self.bodies.into_inner()
    }
}

impl crate::OpaqueInputs for Recorder<'_> {
    fn note(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) {
        self.opaque.note(after, name, span, types)
    }

    fn names(&self, after: bool, name: &str, span: Option<&(String, usize)>, names: &[crate::ExpandedName]) {
        crate::OpaqueInputs::names(&self.opaque, after, name, span, names)
    }

    fn callee(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) -> bool {
        crate::OpaqueInputs::callee(&self.opaque, after, name, span, types)
    }

    fn layout(&self, after: bool, name: &str, span: Option<&(String, usize)>, types: &[String]) -> bool {
        crate::OpaqueInputs::layout(&self.opaque, after, name, span, types)
    }

    fn body(&self, after: bool, name: &str, span: Option<&(String, usize)>, config: usize, target: &str) {
        self.bodies.borrow_mut()[usize::from(after)].entry(name.to_string()).or_default().add(span, config, target);
    }
}

impl Seen {
    /// Adds the record of the body in one unit.
    fn add(&mut self, span: Option<&(String, usize)>, config: usize, target: &str) {
        self.places.entry(span.cloned()).or_default().insert((config, target.to_string()));
    }
}

/// True if the item has the full line: the text before the item is white space, and the text
/// after it is white space or a line comment. A body that starts on the line is then in the
/// item. `text`: the text of the line.
fn full_line(item: &semilla_syntax::Item, line: usize, text: Option<&str>) -> bool {
    if item.line_start < line && line < item.line_end {
        return true;
    }
    let Some(text) = text else { return false };
    let before = line != item.line_start || text.chars().take(item.col_start).all(char::is_whitespace);
    let rest: String = text.chars().skip(item.col_end).collect();
    let after = line != item.line_end || rest.trim_start().is_empty() || rest.trim_start().starts_with("//");
    before && after
}

/// One place of a body in one tree.
struct Place<'a> {
    /// The units with the body at this place: the index of the checked configuration, and the unit.
    units: Vec<(usize, &'a Unit)>,
    /// Each condition of an attribute `#[cfg(P)]` around the place.
    own: Vec<Pred>,
}

/// The data of the check of one step.
pub(crate) struct Joint<'a> {
    pub(crate) sides: [&'a Workspace; 2],
    pub(crate) truths: &'a [Vec<ConfigTruth>; 2],
    pub(crate) bodies: &'a Bodies,
    /// The rename map of the items: the path before the step to the path after it.
    pub(crate) renames: &'a BTreeMap<String, String>,
}

/// The cause that keeps the rule on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Gap {
    /// The single runs find no atom.
    NoAtom,
    /// More atoms than the limit.
    Many(usize),
    /// Semilla cannot find the units of the body.
    NoUnit,
    /// The proof uses a condition that the unit of the body does not evaluate.
    Outside,
    /// A valuation that can occur and that no checked unit has: each atom with its value.
    Valuation(Vec<(String, bool)>),
}

/// The text of an atom for a message: `feature = "fast"`.
fn show(atom: &str) -> String {
    format!("`{}`", cfgcond::show(atom))
}

/// The name of the `dev` configuration of an added selection, as `Configuration::name` gives it.
fn selection_name(selection: &Selection) -> String {
    format!("dev{}, features {}", if selection.no_default { ", no default features" } else { "" }, selection.features.join(","))
}

/// The name of a feature atom.
fn feature(atom: &str) -> Option<&str> {
    atom.strip_prefix("feature=\"")?.strip_suffix('"')
}

/// The plain features that each feature sets: the table of a manifest, with each chain.
fn feature_table(manifest: Option<&String>) -> BTreeMap<String, BTreeSet<String>> {
    let table = manifest.and_then(|text| text.parse::<toml::Table>().ok());
    let direct: BTreeMap<String, BTreeSet<String>> = table
        .as_ref()
        .and_then(|table| table.get("features")?.as_table())
        .map(|features| {
            features
                .iter()
                .map(|(name, list)| {
                    // A plain name is a feature of this package. An entry with `/` or `dep:`
                    // is not a feature of this package.
                    let plain = list.as_array().into_iter().flatten().filter_map(|entry| entry.as_str()).filter(|entry| !entry.contains('/') && !entry.contains(':'));
                    (name.clone(), plain.map(str::to_string).collect())
                })
                .collect()
        })
        .unwrap_or_default();
    let mut all = direct.clone();
    loop {
        let mut grew = false;
        for (name, set) in all.clone() {
            for next in set {
                for more in direct.get(&next).into_iter().flatten() {
                    grew |= all.entry(name.clone()).or_default().insert(more.clone());
                }
            }
        }
        if !grew {
            return all;
        }
    }
}

impl Joint<'_> {
    /// The record name of a body in the tree before the step.
    fn name_before(&self, name: &str) -> String {
        for (old, new) in self.renames {
            if name == new {
                return old.clone();
            }
            if let Some(rest) = name.strip_prefix(new.as_str()).filter(|rest| rest.starts_with("::")) {
                return format!("{old}{rest}");
            }
        }
        name.to_string()
    }

    /// The places of a body in one tree. `None`: a unit has no record.
    fn places(&self, side: usize, name: &str) -> Option<Vec<Place<'_>>> {
        let seen = self.bodies[side].get(name)?;
        let mut places = Vec::new();
        for (span, list) in &seen.places {
            let mut units = Vec::new();
            for (config, target) in list {
                units.push((*config, self.truths[side].get(*config)?.units.iter().find(|unit| unit.target == *target)?));
            }
            places.push(Place { units, own: self.own(side, span.as_ref()) });
        }
        places.iter().any(|place| !place.units.is_empty()).then_some(places)
    }

    /// Each unit of a body in one tree.
    fn units(&self, side: usize, name: &str) -> Option<Vec<&Unit>> {
        Some(self.places(side, name)?.iter().flat_map(|place| place.units.iter().map(|(_, unit)| *unit)).collect())
    }

    /// The manifest of a package folder in one tree.
    fn manifest(&self, side: usize, folder: &str) -> Option<&String> {
        let folder = folder.trim_matches('/');
        let path = if folder.is_empty() || folder == "." { "Cargo.toml".to_string() } else { format!("{folder}/Cargo.toml") };
        self.sides[side].other_files.get(&path)
    }

    /// Each condition `P` of an attribute `#[cfg(P)]` of an item around a line of a file. The
    /// item must have the full line.
    fn own(&self, side: usize, span: Option<&(String, usize)>) -> Vec<Pred> {
        let Some((file, line)) = span else { return Vec::new() };
        let text = self.sides[side].rust_files.get(file).and_then(|source| source.lines().nth(line.saturating_sub(1)));
        let around = self.sides[side].items.values().filter(|item| item.file == *file && item.line_start <= *line && *line <= item.line_end && full_line(item, *line, text));
        around.flat_map(|item| item.attrs.iter().flat_map(|attr| cfgcond::own_conditions(attr))).collect()
    }

    /// The files of the crate of a body: each unit of the body reads them in each tree that
    /// has them, and no unit of a different crate reads them.
    fn files(&self, units: &[Vec<&Unit>; 2], root: &str) -> BTreeSet<String> {
        let mut files: BTreeSet<String> = units.iter().flatten().flat_map(|unit| unit.files.iter().cloned()).collect();
        for side in 0..2 {
            let tree = &self.sides[side].rust_files;
            files.retain(|file| !tree.contains_key(file) || units[side].iter().all(|unit| unit.files.contains(file)));
            for unit in self.truths[side].iter().flat_map(|config| config.units.iter()).filter(|unit| unit.root != root) {
                files.retain(|file| !unit.files.contains(file));
            }
        }
        files
    }

    /// Checks the proof of one body with the atoms `atoms`. `only`: the result of the body
    /// with the rule on for each condition that is not a condition of these atoms in these
    /// files; true if it is the plain result.
    ///
    /// `trees`: the trees with the body. A body of one tree only (an added or a deleted item)
    /// has no second body to compare (batch 5u, part B). Then each valuation must be the
    /// valuation of a checked unit that compiles the body in this tree: the result is a
    /// statement about the body of this tree only.
    ///
    /// `missing` gets each valuation that can occur and that no checked unit has. The gap of
    /// the result is the first of them.
    fn check(&self, name: &str, trees: [bool; 2], atoms: &BTreeSet<String>, missing: &mut Vec<Vec<(String, bool)>>, only: &mut dyn FnMut(&BTreeSet<String>, &BTreeSet<String>) -> bool) -> Result<(), Gap> {
        if atoms.is_empty() {
            return Err(Gap::NoAtom);
        }
        if atoms.len() > LIMIT {
            return Err(Gap::Many(atoms.len()));
        }
        if trees == [false, false] {
            return Err(Gap::NoUnit);
        }
        let side_places = |side: usize, name: &str| if trees[side] { self.places(side, name).ok_or(Gap::NoUnit) } else { Ok(Vec::new()) };
        let mut places = [side_places(0, &self.name_before(name))?, side_places(1, name)?];
        let units: [Vec<&Unit>; 2] = [0, 1].map(|side| places[side].iter().flat_map(|place| place.units.iter().map(|(_, unit)| *unit)).collect());
        // One crate: each unit has the same root file.
        let first = if trees[1] { 1 } else { 0 };
        let root = units[first][0].root.clone();
        if root.is_empty() || units.iter().flatten().any(|unit| unit.root != root) {
            return Err(Gap::NoUnit);
        }
        if !only(atoms, &self.files(&units, &root)) {
            return Err(Gap::Outside);
        }
        // The atoms of the attributes around the body that are not in `A`: they select the
        // place of the body.
        let outside = |places: &[Vec<Place>; 2]| -> BTreeSet<String> {
            let mut all = BTreeSet::new();
            places.iter().flatten().flat_map(|place| place.own.iter()).for_each(|pred| cfgcond::atoms(pred, &mut all));
            all.retain(|atom| !atoms.contains(atom));
            all
        };
        let one_place = (0..2).all(|side| places[side].len() == usize::from(trees[side]));
        let mut extra = outside(&places);
        if atoms.len() + extra.len() > LIMIT {
            if !one_place {
                return Err(Gap::Many(atoms.len() + extra.len()));
            }
            // One place in each tree: an attribute with an atom out of `A` excludes nothing.
            for place in places.iter_mut().flatten() {
                place.own.retain(|pred| {
                    let mut used = BTreeSet::new();
                    cfgcond::atoms(pred, &mut used);
                    used.iter().all(|atom| atoms.contains(atom))
                });
            }
            extra.clear();
        }
        let atoms: Vec<&String> = atoms.iter().collect();
        let extra: Vec<&String> = extra.iter().collect();
        let tables = [0, 1].map(|side| feature_table(units[side].first().and_then(|unit| self.manifest(side, &unit.package))));
        for bits in 0..(1usize << atoms.len()) {
            let values: Vec<bool> = (0..atoms.len()).map(|at| bits >> at & 1 == 1).collect();
            // The checked configurations with a unit that has these values and the body at a place.
            let configs = |place: &Place| -> BTreeSet<usize> {
                place.units.iter().filter(|(_, unit)| atoms.iter().zip(&values).all(|(atom, value)| unit.cfg.contains(*atom) == *value)).map(|(config, _)| *config).collect()
            };
            for more in 0..(1usize << extra.len()) {
                let mut valuation: Vec<(String, bool)> = atoms.iter().map(|atom| (*atom).clone()).zip(values.iter().copied()).collect();
                valuation.extend(extra.iter().enumerate().map(|(at, atom)| ((*atom).clone(), more >> at & 1 == 1)));
                valuation.sort();
                let cfg: BTreeSet<String> = valuation.iter().filter(|(_, value)| *value).map(|(atom, _)| atom.clone()).collect();
                let on = |atom: &str| valuation.iter().find(|(name, _)| name == atom).map(|(_, value)| *value);
                // The feature table excludes the build: a feature is on, and a feature that it sets is off.
                let table = |side: usize| cfg.iter().filter_map(|atom| feature(atom)).any(|name| tables[side].get(name).into_iter().flatten().any(|next| on(&format!("feature=\"{next}\"")) == Some(false)));
                // The places of the body in a build with these values: no attribute is false.
                let found = [0, 1].map(|side| places[side].iter().filter(|place| !place.own.iter().any(|pred| cfgcond::eval(pred, &cfg) == Some(false))).collect::<Vec<_>>());
                // No tree has the body in this build, or the build cannot occur.
                if (0..2).all(|side| !trees[side] || table(side) || found[side].is_empty()) {
                    continue;
                }
                // One checked configuration has the body at these places with these values.
                let checked = if trees == [true, true] {
                    found.iter().all(|side| !side.is_empty()) && found[0].iter().all(|before| found[1].iter().all(|after| !configs(before).is_disjoint(&configs(after))))
                } else {
                    // One tree: a checked unit has the body at each place with these values.
                    found[first].iter().all(|place| !configs(place).is_empty())
                };
                if !checked {
                    // One place in each tree: the atoms of `A` are the gap.
                    valuation.retain(|(atom, _)| !one_place || atoms.contains(&atom));
                    if !missing.contains(&valuation) {
                        missing.push(valuation);
                    }
                }
            }
        }
        match missing.first() {
            Some(first) => Err(Gap::Valuation(first.clone())),
            None => Ok(()),
        }
    }

    /// The text of a gap: the state that no checked unit has, and the fix. `added`: the
    /// feature selections that Semilla added for this step.
    fn text(&self, name: &str, gap: &Gap, added: &[Selection]) -> String {
        match gap {
            Gap::NoAtom | Gap::NoUnit => format!("{GAP}Semilla cannot find the build conditions of this proof"),
            Gap::Many(count) => format!("{GAP}this proof uses {count} build conditions (the limit is {LIMIT})"),
            Gap::Outside => format!("{GAP}this proof uses a build condition of a different crate, or of a file that a different crate also reads"),
            Gap::Valuation(valuation) => {
                let state: Vec<String> = valuation.iter().map(|(atom, value)| format!("{} {}", show(atom), if *value { "on" } else { "off" })).collect();
                let selection = self.selection(name, valuation);
                let fix = match &selection {
                    // The added configuration is in the list, and the gap stays.
                    Some(selection) if added.contains(selection) => format!(". Semilla added the configuration `{}` for this state. The configuration does not build before the step, or it does not give this state", selection_name(selection)),
                    // An entry of `feature-sets` adds its features to the default features.
                    Some(selection) if !selection.no_default => {
                        let set: Vec<String> = selection.features.iter().map(|name| format!("\"{name}\"")).collect();
                        format!(". Add `feature-sets = [[{}]]` to `[workspace.metadata.semilla]`", set.join(", "))
                    }
                    _ => String::new(),
                };
                format!("{GAP}{}{fix}", state.join(" with "))
            }
        }
    }

    /// The feature selection that gives a valuation (batch 5u, part C): the features that are
    /// on, with the default features if they set no feature that is off. `None`: no selection
    /// of this form gives the valuation.
    ///
    /// The result has no part in a proof. It is the configuration that Semilla tries: the
    /// joint check reads the true values of the units of the configuration from the compiler.
    fn selection(&self, name: &str, valuation: &[(String, bool)]) -> Option<Selection> {
        let units = self.units(1, name)?;
        let on: Vec<&str> = valuation.iter().filter(|(_, value)| *value).filter_map(|(atom, _)| feature(atom)).collect();
        let off: Vec<&str> = valuation.iter().filter(|(_, value)| !*value).filter_map(|(atom, _)| feature(atom)).collect();
        if on.is_empty() {
            return None;
        }
        // A checked unit of the body has this feature state: a selection does not help.
        let features: Vec<&(String, bool)> = valuation.iter().filter(|(atom, _)| feature(atom).is_some()).collect();
        if units.iter().any(|unit| features.iter().all(|(atom, value)| unit.cfg.contains(atom) == *value)) {
            return None;
        }
        // The other atoms: a checked unit of the body must have their values.
        let other: Vec<&(String, bool)> = valuation.iter().filter(|(atom, _)| feature(atom).is_none()).collect();
        if !units.iter().any(|unit| other.iter().all(|(atom, value)| unit.cfg.contains(atom) == *value)) {
            return None;
        }
        let table = feature_table(self.manifest(1, &units[0].package));
        let set = |name: &str| -> BTreeSet<String> { table.get(name).into_iter().flatten().cloned().chain([name.to_string()]).collect() };
        let mut listed = BTreeSet::new();
        on.iter().for_each(|name| listed.extend(set(name)));
        if off.iter().any(|name| listed.contains(*name)) {
            return None;
        }
        // The default features stay on if they set no feature that is off.
        let mut default = set("default");
        default.remove("default");
        let no_default = off.iter().any(|name| default.contains(*name));
        Some(Selection { no_default, features: on.iter().map(|name| name.to_string()).collect() })
    }

    /// The result of the step. `plain`: the result with the rule off for each covered
    /// condition. `atoms`: each atom of a covered condition. `run`: the result with a part of
    /// the rule on again.
    ///
    /// `property`: the step has a property claim. A body that is not equal in the plain run
    /// has no proof: each fact of the body has a class that is not `neutral`, and the reviewer
    /// reads it. So the plain result stands for it, as in batch 5s (batch 5u, part B): a
    /// condition that the checked units cover alone asks for no reason. This cannot make a
    /// result "proved": the status of the body stays not equal. A property claim is a result
    /// "proved" that does not come from an equal body, so a step with a property claim keeps
    /// the joint check for each body.
    ///
    /// `added`: the feature selections that Semilla added for this step. The second part of
    /// the result is each other selection that gives a state of a gap (batch 5u, part C), in
    /// the order of the bodies and of their states.
    pub(crate) fn settle(&self, plain: Vec<BodyComparison>, atoms: &BTreeSet<String>, property: bool, added: &[Selection], run: &dyn Fn(&Mode) -> anyhow::Result<Vec<BodyComparison>>) -> anyhow::Result<(Vec<BodyComparison>, Vec<Selection>)> {
        if atoms.is_empty() {
            return Ok((plain, Vec::new()));
        }
        let by_name = |bodies: Vec<BodyComparison>| -> BTreeMap<String, BodyComparison> { bodies.into_iter().map(|body| (body.name.clone(), body)).collect() };
        let strict = by_name(run(&Mode::Strict)?);
        let proved = |body: &BodyComparison| property || matches!(body.status, BodyStatus::Equal { .. });
        let open: Vec<usize> = plain.iter().enumerate().filter(|(_, body)| proved(body) && strict.get(&body.name) != Some(body)).map(|(at, _)| at).collect();
        if open.is_empty() {
            return Ok((plain, Vec::new()));
        }
        let mut single: Vec<(&String, BTreeMap<String, BodyComparison>)> = Vec::new();
        for atom in atoms {
            single.push((atom, by_name(run(&Mode::Without(atom.clone()))?)));
        }
        let mut groups: BTreeMap<(BTreeSet<String>, BTreeSet<String>), Option<BTreeMap<String, BodyComparison>>> = BTreeMap::new();
        let mut out = plain;
        let mut wanted: Vec<Selection> = Vec::new();
        for at in open {
            let body = &out[at];
            let used: BTreeSet<String> = single.iter().filter(|(_, result)| result.get(&body.name) != Some(body)).map(|(atom, _)| (*atom).clone()).collect();
            let mut only = |atoms: &BTreeSet<String>, files: &BTreeSet<String>| {
                let result = groups.entry((atoms.clone(), files.clone())).or_insert_with(|| run(&Mode::Only(atoms.clone(), files.clone())).ok().map(by_name));
                result.as_ref().is_some_and(|result| result.get(&body.name) == Some(body))
            };
            let trees = [body.status != BodyStatus::AfterOnly, body.status != BodyStatus::BeforeOnly];
            let mut missing = Vec::new();
            let Err(gap) = self.check(&body.name, trees, &used, &mut missing, &mut only) else { continue };
            for selection in missing.iter().filter_map(|valuation| self.selection(&body.name, valuation)) {
                if !added.contains(&selection) && !wanted.contains(&selection) {
                    wanted.push(selection);
                }
            }
            let text = self.text(&body.name, &gap, added);
            let mut body = match strict.get(&body.name) {
                Some(body) => body.clone(),
                None => BodyComparison { status: BodyStatus::Different { cfg_changed: true }, ..body.clone() },
            };
            body.config.push(text);
            out[at] = body;
        }
        Ok((out, wanted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIB: &str = "src/lib.rs";
    const BODY: &str = "demo::m::cut";
    const SOURCE: &str = "pub fn top() {}\n#[cfg(feature = \"fc\")]\npub mod m {\n    pub fn cut(a: u64) -> u64 {\n        a\n    }\n}\n";

    fn load(features: &str) -> (tempfile::TempDir, Workspace) {
        load_source(features, SOURCE)
    }

    fn load_source(features: &str, source: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), format!("[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[features]\n{features}")).unwrap();
        std::fs::write(dir.path().join(LIB), source).unwrap();
        let ws = Workspace::load(dir.path()).unwrap();
        (dir, ws)
    }

    fn atom(name: &str) -> String {
        if name.len() == 2 { format!("feature=\"{name}\"") } else { name.to_string() }
    }

    /// One configuration with one library unit for each list of true names.
    fn truths(list: &[&[&str]]) -> Vec<ConfigTruth> {
        let unit = |names: &[&str]| Unit { target: "demo.lib".to_string(), package: String::new(), cfg: names.iter().map(|name| atom(name)).collect(), files: [LIB.to_string()].into(), root: LIB.to_string() };
        list.iter().enumerate().map(|(at, names)| ConfigTruth { name: format!("c{at}"), units: vec![unit(names)], known: true }).collect()
    }

    /// The body `demo::m::cut` at line 4, in each configuration.
    fn bodies(count: usize) -> Bodies {
        let seen = Seen { places: BTreeMap::from([(Some((LIB.to_string(), 4)), (0..count).map(|at| (at, "demo.lib".to_string())).collect())]) };
        [BTreeMap::from([(BODY.to_string(), seen.clone())]), BTreeMap::from([(BODY.to_string(), seen)])]
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| atom(name)).collect()
    }

    /// The result of the check for a feature table, a list of units, and a set of atoms.
    fn check(features: &str, list: &[&[&str]], atoms: &[&str]) -> Result<(), Gap> {
        let (_dir, ws) = load(features);
        let truths = [truths(list), truths(list)];
        let bodies = bodies(list.len());
        let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
        joint.check(BODY, [true, true], &set(atoms), &mut Vec::new(), &mut |_, files| files.contains(LIB))
    }

    fn gap(valuation: &[(&str, bool)]) -> Result<(), Gap> {
        Err(Gap::Valuation(valuation.iter().map(|(name, value)| (atom(name), *value)).collect()))
    }

    #[test]
    fn the_feature_table_has_each_chain() {
        let manifest = "[features]\ndefault = [\"std\"]\nstd = [\"alloc\"]\nalloc = []\nfull = [\"std\", \"dep:serde\", \"serde/derive\"]\n".to_string();
        let table = feature_table(Some(&manifest));
        let names = |name: &str| table[name].iter().map(String::as_str).collect::<Vec<_>>();
        assert_eq!(names("default"), ["alloc", "std"]);
        assert_eq!(names("full"), ["alloc", "std"]);
        assert!(table["alloc"].is_empty() && feature_table(None).is_empty());
        assert!(feature_table(Some(&"not a manifest [".to_string())).is_empty());
    }

    #[test]
    fn each_valuation_that_can_occur_must_be_the_valuation_of_a_checked_unit() {
        let table = "fa = []\nfb = []\nfc = []\n";
        // Probe d11e: each feature is covered, and the list has no unit with only one of them.
        assert_eq!(check(table, &[&[], &["fa", "fb"]], &["fa", "fb"]), gap(&[("fa", true), ("fb", false)]));
        assert_eq!(check(table, &[&[], &["fa", "fb"], &["fa"]], &["fa", "fb"]), gap(&[("fa", false), ("fb", true)]));
        assert_eq!(check(table, &[&[], &["fa", "fb"], &["fa"], &["fb"]], &["fa", "fb"]), Ok(()));
        // One atom: the rule of batch 5s.
        assert_eq!(check(table, &[&[], &["fa", "fb"]], &["fa"]), Ok(()));
        assert_eq!(check(table, &[&["fa"], &["fa", "fb"]], &["fa"]), gap(&[("fa", false)]));
        // A profile atom with a feature: four valuations.
        let three: &[&[&str]] = &[&["debug_assertions"], &[], &["debug_assertions", "fa"]];
        assert_eq!(check(table, three, &["debug_assertions", "fa"]), gap(&[("debug_assertions", false), ("fa", true)]));
        assert_eq!(check(table, &[three[0], three[1], three[2], &["fa"]], &["debug_assertions", "fa"]), Ok(()));
    }

    #[test]
    fn only_an_exact_cause_excludes_a_valuation() {
        // The feature table: `fa` sets `fb`, also through a chain.
        assert_eq!(check("fa = [\"fb\"]\nfb = []\n", &[&[], &["fa", "fb"]], &["fa", "fb"]), gap(&[("fa", false), ("fb", true)]));
        assert_eq!(check("fa = [\"fb\"]\nfb = []\n", &[&[], &["fa", "fb"], &["fb"]], &["fa", "fb"]), Ok(()));
        assert_eq!(check("fa = [\"fx\"]\nfx = [\"fb\"]\nfb = []\n", &[&[], &["fa", "fb"], &["fb"]], &["fa", "fb"]), Ok(()));
        // An entry for a feature of a dependency excludes no valuation.
        assert_eq!(check("fa = [\"dep/fb\"]\nfb = []\n", &[&[], &["fa", "fb"], &["fb"]], &["fa", "fb"]), gap(&[("fa", true), ("fb", false)]));
        // The module of the body has `#[cfg(feature = "fc")]`: no build with `fc` off has the body.
        assert_eq!(check("fa = []\nfc = []\n", &[&["fc"], &["fa", "fc"]], &["fa", "fc"]), Ok(()));
        assert_eq!(check("fa = []\nfc = []\n", &[&["fa", "fc"]], &["fa", "fc"]), gap(&[("fa", false), ("fc", true)]));
        // The condition of the module has an atom out of the set: a build with `fc` on has the body.
        assert_eq!(check("fa = []\nfb = []\n", &[&["fc"], &["fa", "fb", "fc"]], &["fa", "fb"]), gap(&[("fa", true), ("fb", false)]));
    }

    /// The check of a body at a line of a source text. Each checked unit has `fc`. The
    /// result is `Ok` only if a false `#[cfg(feature = "fc")]` excludes the builds with no `fc`.
    fn check_at(source: &str, line: usize) -> Result<(), Gap> {
        let (_dir, ws) = load_source("fa = []\nfc = []\n", source);
        let list: &[&[&str]] = &[&["fc"], &["fa", "fc"]];
        let truths = [truths(list), truths(list)];
        let mut bodies = bodies(list.len());
        for side in &mut bodies {
            let seen = side.get_mut(BODY).unwrap();
            let units = seen.places.pop_first().unwrap().1;
            seen.places.insert(Some((LIB.to_string(), line)), units);
        }
        let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
        joint.check(BODY, [true, true], &set(&["fa", "fc"]), &mut Vec::new(), &mut |_, files| files.contains(LIB))
    }

    #[test]
    fn a_false_attribute_excludes_a_valuation_only_with_the_full_line() {
        let open = gap(&[("fa", false), ("fc", false)]);
        assert_eq!(check_at(SOURCE, 4), Ok(()));
        // Probe w2: an item under `cfg` is on the line of the body. The body is in each build.
        assert_eq!(check_at("#[cfg(feature = \"fc\")] pub fn z() {} pub fn cut(a: u64) -> u64 { a }\n", 1), open);
        // Probe w2e: a module under `cfg` ends on the line of the body.
        assert_eq!(check_at("#[cfg(feature = \"fc\")]\npub mod z {\n} pub fn cut(a: u64) -> u64 { a }\n", 3), open);
        // Probe w2s: a module under `cfg` starts on the line of the body.
        let after = "pub fn cut(a: u64) -> u64 { a } #[cfg(feature = \"fc\")] pub mod z {\n    pub fn in_z() {}\n}\n";
        assert_eq!(check_at(after, 1), open);
        assert_eq!(check_at(after, 2), Ok(()));
        // The item has the full line: the attribute is on the line, and a line comment is after it.
        assert_eq!(check_at("#[cfg(feature = \"fc\")] pub fn cut(a: u64) -> u64 { a } // one line\n", 1), Ok(()));
        assert_eq!(check_at("pub fn top() {}\n    #[cfg(feature = \"fc\")]\n    pub fn cut(a: u64) -> u64 { a }\n", 3), Ok(()));
        assert_eq!(check_at("#[cfg(feature = \"fc\")] pub fn cut(a: u64) -> u64 { a } /* c */ pub fn top() {}\n", 1), open);
        // A line that the tree does not have gives no exclusion.
        assert_eq!(check_at("#[cfg(feature = \"fc\")] pub fn cut(a: u64) -> u64 { a }\n", 7), open);
    }

    #[test]
    fn an_attribute_with_an_atom_out_of_the_set_selects_the_builds_with_the_body() {
        // The module of the body has `#[cfg(feature = "fc")]`, and the proof uses `fa` only.
        assert_eq!(check("fa = []\nfc = []\n", &[&["fc"], &["fa", "fc"]], &["fa"]), Ok(()));
        assert_eq!(check("fa = []\nfc = []\n", &[&["fa", "fc"]], &["fa"]), gap(&[("fa", false)]));
        // `fc` sets `fa`: no build has the body with `fa` off.
        assert_eq!(check("fa = []\nfc = [\"fa\"]\n", &[&["fa", "fc"]], &["fa"]), Ok(()));
    }

    /// Probe w1: two twins of a body, under `not(fa)` at line 3 and under `fa` at line 5. Each
    /// entry of the list is the place and the true names of one checked configuration.
    fn check_twins(list: &[(usize, &[&str])], atoms: &[&str]) -> Result<(), Gap> {
        let source = "pub fn top() {}\n#[cfg(not(feature = \"fa\"))]\npub fn cut(a: u64) -> u64 { a }\n#[cfg(feature = \"fa\")]\npub fn cut(a: u64) -> u64 { a + 1 }\n";
        let (_dir, ws) = load_source("fa = []\nfb = [\"fa\"]\n", source);
        let names: Vec<&[&str]> = list.iter().map(|(_, names)| *names).collect();
        let truths = [truths(&names), truths(&names)];
        let mut seen = Seen::default();
        for (at, (line, _)) in list.iter().enumerate() {
            seen.add(Some(&(LIB.to_string(), *line)), at, "demo.lib");
        }
        let bodies = [BTreeMap::from([(BODY.to_string(), seen.clone())]), BTreeMap::from([(BODY.to_string(), seen)])];
        let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
        joint.check(BODY, [true, true], &set(atoms), &mut Vec::new(), &mut |_, files| files.contains(LIB))
    }

    #[test]
    fn each_twin_of_a_body_needs_its_own_checked_units() {
        // The proof uses `fb` only. The unit with `fb` off has the twin under `not(fa)`: it
        // is not a check of the twin under `fa`. `fb` sets `fa`.
        assert_eq!(check_twins(&[(3, &[]), (5, &["fa", "fb"])], &["fb"]), gap(&[("fa", true), ("fb", false)]));
        assert_eq!(check_twins(&[(3, &[]), (5, &["fa", "fb"]), (5, &["fa"])], &["fb"]), Ok(()));
        // The two atoms in the set give the same result.
        assert_eq!(check_twins(&[(3, &[]), (5, &["fa", "fb"])], &["fa", "fb"]), gap(&[("fa", true), ("fb", false)]));
        // Only one twin has a unit: the other twin has no place.
        assert_eq!(check_twins(&[(5, &["fa", "fb"]), (5, &["fa"])], &["fb"]), Ok(()));
        // The units with one name at two places are two places.
        let mut seen = Seen::default();
        seen.add(Some(&(LIB.to_string(), 3)), 0, "demo.lib");
        seen.add(Some(&(LIB.to_string(), 3)), 1, "demo.lib");
        seen.add(Some(&(LIB.to_string(), 5)), 2, "demo.lib");
        seen.add(None, 3, "demo.lib");
        assert_eq!(seen.places.values().map(BTreeSet::len).collect::<Vec<_>>(), [1, 2, 1]);
    }

    #[test]
    fn one_checked_configuration_must_have_the_valuation_in_the_two_trees() {
        let (_dir, ws) = load("fa = []\n");
        let bodies = bodies(2);
        let same = [truths(&[&[], &["fa"]]), truths(&[&[], &["fa"]])];
        let joint = Joint { sides: [&ws, &ws], truths: &same, bodies: &bodies, renames: &BTreeMap::new() };
        assert_eq!(joint.check(BODY, [true, true], &set(&["fa"]), &mut Vec::new(), &mut |_, _| true), Ok(()));
        // The step changes the features of each configuration: no configuration has `fa` off in the two trees.
        let moved = [truths(&[&[], &["fa"]]), truths(&[&["fa"], &[]])];
        let joint = Joint { truths: &moved, ..joint };
        assert_eq!(joint.check(BODY, [true, true], &set(&["fa"]), &mut Vec::new(), &mut |_, _| true), gap(&[("fa", false)]));
    }

    #[test]
    fn the_rule_stays_on_with_no_exact_answer() {
        let table = "fa = []\nfb = []\n";
        let list: &[&[&str]] = &[&[], &["fa"]];
        assert_eq!(check(table, list, &[]), Err(Gap::NoAtom));
        assert_eq!(check(table, list, &["f1", "f2", "f3", "f4", "f5", "f6", "f7"]), Err(Gap::Many(7)));
        let (_dir, ws) = load(table);
        let sides = [truths(list), truths(list)];
        let seen = bodies(2);
        let joint = Joint { sides: [&ws, &ws], truths: &sides, bodies: &seen, renames: &BTreeMap::new() };
        // The proof uses a condition that the unit of the body does not evaluate.
        assert_eq!(joint.check(BODY, [true, true], &set(&["fa"]), &mut Vec::new(), &mut |_, _| false), Err(Gap::Outside));
        // A body with no record of its units.
        assert_eq!(joint.check("demo::none", [true, true], &set(&["fa"]), &mut Vec::new(), &mut |_, _| true), Err(Gap::NoUnit));
        // The units of the body are units of two crates.
        let mut two = sides.clone();
        two[1][1].units[0].root = "tests/it.rs".to_string();
        let joint = Joint { truths: &two, ..joint };
        assert_eq!(joint.check(BODY, [true, true], &set(&["fa"]), &mut Vec::new(), &mut |_, _| true), Err(Gap::NoUnit));
        // A unit of a different crate reads the file: the file is not a file of one crate.
        let mut shared = sides.clone();
        let other = Unit { target: "it.test".to_string(), root: "tests/it.rs".to_string(), ..shared[0][0].units[0].clone() };
        shared[0][0].units.push(other);
        let joint = Joint { truths: &shared, ..joint };
        assert_eq!(joint.check(BODY, [true, true], &set(&["fa"]), &mut Vec::new(), &mut |_, files| files.contains(LIB)), Err(Gap::Outside));
    }

    #[test]
    fn a_proof_with_a_gap_gets_the_strict_result_and_the_gap_text() {
        let (_dir, ws) = load("fa = []\nfb = []\n");
        let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), span: None };
        let equal = |level: &str| BodyStatus::Equal { level: level.to_string(), proof: Vec::new() };
        let plain = vec![body(BODY, equal("n3")), body("demo::top", equal("n0"))];
        let stopped = vec![body(BODY, BodyStatus::Different { cfg_changed: false }), body("demo::top", equal("n0"))];
        let runs = RefCell::new(Vec::new());
        let run = |mode: &Mode| -> anyhow::Result<Vec<BodyComparison>> {
            runs.borrow_mut().push(mode.clone());
            Ok(match mode {
                Mode::Plain | Mode::Only(..) => plain.clone(),
                Mode::Strict | Mode::Without(_) => stopped.clone(),
            })
        };
        let settle = |list: &[&[&str]]| {
            let truths = [truths(list), truths(list)];
            let bodies = bodies(list.len());
            let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
            joint.settle(plain.clone(), &set(&["fa", "fb"]), false, &[], &run).unwrap().0
        };
        let out = settle(&[&[], &["fa", "fb"]]);
        assert_eq!(out[0].status, BodyStatus::Different { cfg_changed: false });
        assert_eq!(out[0].config, ["not checked together: `feature = \"fa\"` on with `feature = \"fb\"` off. Add `feature-sets = [[\"fa\"]]` to `[workspace.metadata.semilla]`"]);
        assert_eq!(out[1], plain[1]);
        assert_eq!(runs.borrow().len(), 4, "{:?}", runs.borrow());
        // Each valuation is in the list: the plain result stands.
        assert_eq!(settle(&[&[], &["fa", "fb"], &["fa"], &["fb"]]), plain);
        // No covered atom, or a strict result that is equal: no more runs.
        let truths = [truths(&[&[]]), truths(&[&[]])];
        let bodies = bodies(1);
        let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
        runs.borrow_mut().clear();
        assert_eq!(joint.settle(plain.clone(), &BTreeSet::new(), false, &[], &run).unwrap().0, plain);
        assert!(runs.borrow().is_empty());
        assert_eq!(joint.settle(stopped.clone(), &set(&["fa"]), false, &[], &run).unwrap().0, stopped);
        assert_eq!(*runs.borrow(), [Mode::Strict]);
    }

    #[test]
    fn a_body_of_one_tree_needs_each_valuation_in_this_tree() {
        // Batch 5u, part B: an added item has no body before the step.
        let (_dir, ws) = load("fa = []\nfc = []\n");
        let seen = bodies(2);
        let after_only = [BTreeMap::new(), seen[1].clone()];
        let full = truths(&[&["fc"], &["fa", "fc"]]);
        let part = truths(&[&["fa", "fc"], &["fa", "fc"]]);
        let sides = [part.clone(), full.clone()];
        let joint = Joint { sides: [&ws, &ws], truths: &sides, bodies: &after_only, renames: &BTreeMap::new() };
        // The tree after the step has each state. The tree before the step has no part.
        assert_eq!(joint.check(BODY, [false, true], &set(&["fa"]), &mut Vec::new(), &mut |_, _| true), Ok(()));
        // The state is only in the tree with no body: the gap stays.
        let sides = [full, part];
        let joint = Joint { truths: &sides, ..joint };
        let mut missing = Vec::new();
        assert_eq!(joint.check(BODY, [false, true], &set(&["fa"]), &mut missing, &mut |_, _| true), gap(&[("fa", false)]));
        assert_eq!(missing.len(), 1);
        // No tree has the body: no answer.
        assert_eq!(joint.check(BODY, [false, false], &set(&["fa"]), &mut Vec::new(), &mut |_, _| true), Err(Gap::NoUnit));
        // The check of two trees needs the record of the body in each tree.
        assert_eq!(joint.check(BODY, [true, true], &set(&["fa"]), &mut Vec::new(), &mut |_, _| true), Err(Gap::NoUnit));
    }

    #[test]
    fn a_body_with_no_proof_keeps_the_plain_result() {
        // Batch 5u, part B: the body is not equal in the plain run. The rule changes only the
        // flag of the status. The list has a gap for the two atoms.
        let (_dir, ws) = load("fa = []\nfb = []\n");
        let body = |status: BodyStatus| BodyComparison { name: BODY.to_string(), status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), span: None };
        let list: &[&[&str]] = &[&[], &["fa", "fb"]];
        let truths = [truths(list), truths(list)];
        let bodies = bodies(list.len());
        let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
        for status in [BodyStatus::Different { cfg_changed: false }, BodyStatus::AfterOnly, BodyStatus::BeforeOnly] {
            let plain = vec![body(status.clone())];
            let strict = vec![body(BodyStatus::Different { cfg_changed: true })];
            let runs = RefCell::new(Vec::new());
            let run = |mode: &Mode| -> anyhow::Result<Vec<BodyComparison>> {
                runs.borrow_mut().push(mode.clone());
                Ok(if matches!(mode, Mode::Plain | Mode::Only(..)) { plain.clone() } else { strict.clone() })
            };
            // No property claim: the plain result stands, with no gap and no selection.
            let (out, wanted) = joint.settle(plain.clone(), &set(&["fa", "fb"]), false, &[], &run).unwrap();
            assert_eq!((out, wanted), (plain.clone(), Vec::new()), "{status:?}");
            assert_eq!(*runs.borrow(), [Mode::Strict]);
            // A property claim can make the body "proved": the joint check stays.
            let (out, wanted) = joint.settle(plain.clone(), &set(&["fa", "fb"]), true, &[], &run).unwrap();
            assert_eq!(out[0].status, BodyStatus::Different { cfg_changed: true }, "{status:?}");
            assert!(out[0].config[0].starts_with(GAP), "{:?}", out[0].config);
            assert!(!wanted.is_empty(), "{status:?}");
        }
    }

    #[test]
    fn a_selection_is_the_features_that_are_on() {
        // Batch 5u, part C.
        let pick = |features: &str, list: &[&[&str]], valuation: &[(&str, bool)]| {
            let (_dir, ws) = load(features);
            let truths = [truths(list), truths(list)];
            let bodies = bodies(list.len());
            let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
            let valuation: Vec<(String, bool)> = valuation.iter().map(|(name, value)| (atom(name), *value)).collect();
            joint.selection(BODY, &valuation)
        };
        let some = |no_default: bool, features: &[&str]| Some(Selection { no_default, features: features.iter().map(|name| name.to_string()).collect() });
        let table = "fa = []\nfb = []\nfc = []\n";
        let list: &[&[&str]] = &[&[], &["fa", "fb", "fc"]];
        assert_eq!(pick(table, list, &[("fa", true), ("fb", false)]), some(false, &["fa"]));
        assert_eq!(pick(table, list, &[("fa", true), ("fb", false), ("fc", true)]), some(false, &["fa", "fc"]));
        // No feature is on: the build with no default features is a part of the default list.
        assert_eq!(pick(table, list, &[("fa", false), ("fb", false)]), None);
        // A checked unit has the state: a selection does not help.
        assert_eq!(pick(table, &[&[], &["fa"]], &[("fa", true), ("fb", false)]), None);
        // A feature that is on sets a feature that is off: no build has the state.
        assert_eq!(pick("fa = [\"fb\"]\nfb = []\n", list, &[("fa", true), ("fb", false)]), None);
        // A default feature is off: the selection has no default features.
        assert_eq!(pick("default = [\"fb\"]\nfa = []\nfb = []\n", list, &[("fa", true), ("fb", false)]), some(true, &["fa"]));
        assert_eq!(pick("default = [\"fc\"]\nfa = []\nfb = []\nfc = []\n", list, &[("fa", true), ("fb", false)]), some(false, &["fa"]));
        // An atom that is not a feature: a selection cannot set it. A checked unit must have it.
        assert_eq!(pick(table, list, &[("fa", true), ("fb", false), ("unix", true)]), None);
        assert_eq!(pick(table, &[&["unix"], &["fa", "fb", "unix"]], &[("fa", true), ("fb", false), ("unix", true)]), some(false, &["fa"]));
        assert_eq!(selection_name(&some(true, &["fa", "fc"]).unwrap()), "dev, no default features, features fa,fc");
        assert_eq!(selection_name(&some(false, &["fa"]).unwrap()), "dev, features fa");
    }

    #[test]
    fn a_gap_asks_for_each_selection_one_time() {
        // Batch 5u, part C: the list of the selections for the caller, and the text of a gap
        // that stays with the added configuration.
        let (_dir, ws) = load("fa = []\nfb = []\n");
        let body = |name: &str, status: BodyStatus| BodyComparison { name: name.to_string(), status, config: Vec::new(), differs_in: Vec::new(), equal_in: Vec::new(), stopped: Vec::new(), span: None };
        let equal = BodyStatus::Equal { level: "n3".to_string(), proof: Vec::new() };
        let plain = vec![body(BODY, equal)];
        let stopped = vec![body(BODY, BodyStatus::Different { cfg_changed: false })];
        let run = |mode: &Mode| -> anyhow::Result<Vec<BodyComparison>> {
            Ok(match mode {
                Mode::Plain | Mode::Only(..) => plain.clone(),
                Mode::Strict | Mode::Without(_) => stopped.clone(),
            })
        };
        let list: &[&[&str]] = &[&[], &["fa", "fb"]];
        let truths = [truths(list), truths(list)];
        let bodies = bodies(list.len());
        let joint = Joint { sides: [&ws, &ws], truths: &truths, bodies: &bodies, renames: &BTreeMap::new() };
        let pick = |name: &str| Selection { no_default: false, features: vec![name.to_string()] };
        let (out, wanted) = joint.settle(plain.clone(), &set(&["fa", "fb"]), false, &[], &run).unwrap();
        assert_eq!(wanted, [pick("fa"), pick("fb")]);
        assert!(out[0].config[0].contains("Add `feature-sets = [[\"fa\"]]`"), "{:?}", out[0].config);
        // The caller added the first selection, and the units did not change: the selection
        // is not in the list again, and the text gives the cause. The body has no proof.
        let (out, wanted) = joint.settle(plain.clone(), &set(&["fa", "fb"]), false, &[pick("fa")], &run).unwrap();
        assert_eq!(wanted, [pick("fb")]);
        assert_eq!(out[0].status, BodyStatus::Different { cfg_changed: false });
        assert_eq!(out[0].config, ["not checked together: `feature = \"fa\"` on with `feature = \"fb\"` off. Semilla added the configuration `dev, features fa` for this state. The configuration does not build before the step, or it does not give this state"]);
    }
}
