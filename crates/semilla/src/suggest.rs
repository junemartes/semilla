//! [R75.1-1] The suggested test of each sure difference (design 75). A child module of the
//! oracle.
//!
//! The decision of the user (2026-11-26): "I think the probe can suggest a test to write to
//! detect the difference. It should not manually do it." Semilla never builds or runs the code
//! of the user in a step: the model gives the input and the outcomes, and this module gives each
//! difference the text of a test (`semilla_core::run::suggest`) with its place and its command.

use semilla_core::run::{TestNote, TestPlace};

use super::NormalOracle;

impl NormalOracle {
    /// [R75.1-1] Makes the suggested test of each open result. Each item is a verdict with its
    /// configuration and the old tree of its sides.
    pub(crate) fn suggest(&self, step: &semilla_core::Step, items: &mut [(&str, &str, &str, &mut semilla_verify::equiv::Verdict)]) {
        use semilla_verify::equiv::Verdict;
        let checked = self.checked(step).unwrap_or_default();
        for (config, before, _, verdict) in items.iter_mut() {
            let Verdict::Different { before: old, after: new, test, .. } = &mut **verdict else { continue };
            let TestNote::Open(case) = test else { continue };
            let flags = checked.iter().find(|known| known.name() == *config).map(|known| known.cargo_args().join(" ")).unwrap_or_default();
            let package = self.package_of(before, &checked, case.calls.first().map(|call| call.target.as_str()).unwrap_or_default());
            let place = TestPlace { flags: &flags, package: &package, before: old, after: new };
            *test = match semilla_core::run::suggest(case, &place) {
                Ok(found) => TestNote::Test(found),
                Err(cause) => TestNote::NoTest(cause),
            };
        }
    }

    /// [R75.1-3] The suggested test of a false claim: the test of its counterexample.
    pub(crate) fn suggest_claim(&self, step: &semilla_core::Step, config: &str, case: &semilla_core::run::RunCase, detail: &str) -> TestNote {
        let checked = self.checked(step).unwrap_or_default();
        let flags = checked.iter().find(|known| known.name() == config).map(|known| known.cargo_args().join(" ")).unwrap_or_default();
        let package = self.package_of(&step.tree_before, &checked, case.calls.first().map(|call| call.target.as_str()).unwrap_or_default());
        let place = TestPlace { flags: &flags, package: &package, before: "", after: detail };
        match semilla_core::run::suggest_claim(case, &place) {
            Ok(found) => TestNote::Test(found),
            Err(cause) => TestNote::NoTest(cause),
        }
    }

    /// The package of a compiled target of a tree (from the unit records of a configuration).
    fn package_of(&self, tree: &str, checked: &[semilla_normal::cargo::Configuration], target: &str) -> String {
        checked
            .iter()
            .filter_map(|config| self.units(tree, config))
            .flatten()
            .find(|unit| unit.target == target)
            .map(|unit| unit.package)
            .filter(|package| !package.is_empty())
            .unwrap_or_else(|| target.split('.').next().unwrap_or("PACKAGE").replace('_', "-"))
    }
}
