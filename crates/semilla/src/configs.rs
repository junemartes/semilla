//! The list of the build configurations of a step (design 85, batch 14c).
//!
//! The default list has one configuration: `dev` with the default features. The options of a
//! command give a different list: the words of cargo for one configuration (`--release`,
//! `--features LIST`, `--all-features`, `--no-default-features`), the printed name of a
//! configuration (`--configuration NAME`, it can repeat), or `--configuration full` (the full
//! list of design 8.12). A setting of the manifest gives the default list of a repository:
//! `configurations = ["dev", "release, all features"]` in `[workspace.metadata.semilla]` or
//! `[package.metadata.semilla]`. An option replaces the setting.

use semilla_normal::cargo::{Configuration, Features};

/// The entry of the full list of design 8.12.
pub const FULL: &str = "full";

/// [R85.3-2] The list of the options of a reviewer command (design 85): it replaces the list of
/// each step for the checks of this process. `None`: each step has its own list.
static OVERRIDE: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// Sets the list of the options of a reviewer command for this process.
pub fn set_override(list: Option<Vec<String>>) {
    *OVERRIDE.lock().unwrap_or_else(|error| error.into_inner()) = list;
}

/// The list of the options of a reviewer command, if one exists.
pub fn override_list() -> Option<Vec<String>> {
    OVERRIDE.lock().unwrap_or_else(|error| error.into_inner()).clone()
}

/// [R85.3-2] A change set with the list of the options of a reviewer command in each step (a
/// copy). With no such list, the change set as it is.
pub fn listed(set: &semilla_core::ChangeSet) -> semilla_core::ChangeSet {
    let mut copy = set.clone();
    if let Some(list) = override_list() {
        for step in &mut copy.steps {
            step.configurations = Some(list.clone());
        }
    }
    copy
}

/// [R85.3-2] True if the list of the options of a reviewer command is not the list of each
/// step of a change set: the output then says so.
pub fn overrides(set: &semilla_core::ChangeSet) -> Option<Vec<String>> {
    let list = override_list()?;
    set.steps.iter().any(|step| step.configurations.as_ref() != Some(&list)).then_some(list)
}

/// The options of one command for the list of the build configurations.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub release: bool,
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub configurations: Vec<String>,
}

/// [R85.2-2] The error of a wrong list. The line gives the names that exist.
pub fn form_error(text: &str) -> anyhow::Error {
    anyhow::anyhow!("{text}. The names have the form `dev`, `release`, `dev, no default features`, `dev, all features`, `dev, features a,b`, `release, no default features, features a` (as each output prints them), or `full` (the full list)")
}

impl Options {
    /// True if the command has no option of the list.
    pub fn is_empty(&self) -> bool {
        *self == Options::default()
    }

    /// [R85.2-1] The entries of the list of the options, in a fixed order: `full`, then each
    /// configuration in the order of the full list. `None`: no option. The words of cargo give
    /// one configuration, as for `cargo build`. A wrong name or a pair of words that Semilla
    /// cannot give to cargo is an error.
    pub fn list(&self) -> anyhow::Result<Option<Vec<String>>> {
        if self.is_empty() {
            return Ok(None);
        }
        let mut configs: Vec<Configuration> = Vec::new();
        let mut full = false;
        let cargo_words = self.release || !self.features.is_empty() || self.all_features || self.no_default_features;
        if cargo_words {
            let features: Vec<String> = self.features.iter().flat_map(|list| list.split([',', ' '])).map(str::trim).filter(|name| !name.is_empty()).map(str::to_string).collect();
            if self.all_features && (self.no_default_features || !features.is_empty()) {
                return Err(form_error("the option `--all-features` selects each feature: give it with no `--features` and no `--no-default-features`"));
            }
            if let Some(bad) = features.iter().find(|name| !is_feature_name(name)) {
                return Err(form_error(&format!("`{bad}` is not a feature name")));
            }
            let mut features = features;
            features.sort();
            features.dedup();
            let selection = match (self.all_features, self.no_default_features, features.is_empty()) {
                (true, _, _) => Features::All,
                (false, true, true) => Features::NoDefault,
                (false, true, false) => Features::NoDefaultListed(features),
                (false, false, true) => Features::Default,
                (false, false, false) => Features::Listed(features),
            };
            configs.push(Configuration::new(self.release, selection));
        }
        for name in &self.configurations {
            if name.trim() == FULL {
                full = true;
                continue;
            }
            configs.push(parse_name(name).ok_or_else(|| form_error(&format!("`{}` is not the name of a configuration", name.trim())))?);
        }
        Ok(Some(entries(full, configs)))
    }
}

/// The entries of a list: `full` first, then each configuration once, in the order of the full
/// list (the feature selection, then the profile).
fn entries(full: bool, mut configs: Vec<Configuration>) -> Vec<String> {
    configs.sort_by(|a, b| (&a.features, a.release).cmp(&(&b.features, b.release)));
    configs.dedup();
    full.then(|| FULL.to_string()).into_iter().chain(configs.iter().map(Configuration::name)).collect()
}

/// True for a feature name of cargo: letters, digits, `_`, `-`, `+`, `.`, and one `/` for a
/// feature of a dependency.
fn is_feature_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_-+./:".contains(c)) && name.matches('/').count() <= 1
}

/// [R85.2-1] The configuration of a printed name: `dev`, `release, all features`,
/// `dev, features a,b` (also `a+b`), `release, no default features, features a`. The end
/// " (libraries and binaries only)" of a smaller build is not a part of the name: Semilla
/// selects the smaller build itself. `None`: the text is not such a name.
pub fn parse_name(name: &str) -> Option<Configuration> {
    let name = name.trim();
    let name = name.strip_suffix(Configuration::NARROW).unwrap_or(name).trim();
    let (release, rest) = if let Some(rest) = name.strip_prefix("release") {
        (true, rest)
    } else {
        (false, name.strip_prefix("dev")?)
    };
    let mut no_default = false;
    let mut all = false;
    let mut listed: Option<Vec<String>> = None;
    let mut rest = rest;
    while !rest.is_empty() {
        rest = rest.strip_prefix(',')?.trim_start();
        if let Some(tail) = rest.strip_prefix("no default features") {
            if no_default {
                return None;
            }
            no_default = true;
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("all features") {
            if all {
                return None;
            }
            all = true;
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("features ") {
            // The list is the last part of a name.
            let mut features: Vec<String> = tail.split(['+', ',']).map(str::trim).filter(|name| !name.is_empty()).map(str::to_string).collect();
            if features.is_empty() || listed.is_some() || features.iter().any(|name| !is_feature_name(name)) {
                return None;
            }
            features.sort();
            features.dedup();
            listed = Some(features);
            rest = "";
        } else {
            return None;
        }
    }
    let features = match (all, no_default, listed) {
        (true, false, None) => Features::All,
        (true, _, _) => return None,
        (false, true, None) => Features::NoDefault,
        (false, true, Some(list)) => Features::NoDefaultListed(list),
        (false, false, None) => Features::Default,
        (false, false, Some(list)) => Features::Listed(list),
    };
    Some(Configuration::new(release, features))
}

/// [R85.4-2] The options of the command line that check a configuration, in the words of cargo:
/// `--release`, `--features a,b`, `--no-default-features`, `--all-features`.
pub fn option_of(config: &Configuration) -> String {
    let words = config.cargo_args();
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;
    while index < words.len() {
        if words[index] == "--features" && index + 1 < words.len() {
            out.push(format!("--features {}", words[index + 1]));
            index += 2;
        } else {
            out.push(words[index].clone());
            index += 1;
        }
    }
    if out.is_empty() { format!("--configuration {}", quote(&config.name())) } else { out.join(" ") }
}

/// The text of a command line word in single quotes, if it needs them.
pub fn quote(text: &str) -> String {
    if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || "-_.,+/:=".contains(c)) { text.to_string() } else { format!("'{}'", text.replace('\'', "'\\''")) }
}

/// [R85.2-1] The options of a command line that give a list (for a suggested command): one
/// `--configuration NAME` for each entry. An empty list gives no option.
pub fn command_options(list: &[String]) -> Vec<String> {
    list.iter().map(|entry| format!("--configuration {}", quote(entry))).collect()
}

/// [R85.2-3] The setting `configurations` of a manifest text: the list of
/// `[workspace.metadata.semilla]`, else of `[package.metadata.semilla]`. `None`: the manifest has
/// no such setting. A value that is not a list of texts is an error.
pub fn setting(manifest: &str) -> anyhow::Result<Option<Vec<String>>> {
    let Ok(table) = manifest.parse::<toml::Table>() else { return Ok(None) };
    let found = ["workspace", "package"].iter().find_map(|section| table.get(*section)?.get("metadata")?.get("semilla")?.get("configurations").cloned());
    let Some(value) = found else { return Ok(None) };
    let list = value.as_array().ok_or_else(|| form_error("the setting `configurations` of the manifest is not a list"))?;
    let mut names = Vec::new();
    for entry in list {
        names.push(entry.as_str().ok_or_else(|| form_error("an entry of the setting `configurations` of the manifest is not a text"))?.to_string());
    }
    let options = Options { configurations: names, ..Options::default() };
    let list = options.list().map_err(|error| anyhow::anyhow!("the setting `configurations` of the manifest: {error}"))?;
    Ok(list.filter(|list| !list.is_empty()))
}

/// [R85.1-1] The resolved request of a step: `None` for the full list, else the configurations.
/// `stored`: the list of the step. `setting`: the setting of the manifests of the trees of the
/// step. A stored step with no list (an earlier version) has the full list. An empty list has
/// the setting, else `dev`.
pub fn resolve(stored: Option<&[String]>, setting: Option<Vec<String>>) -> anyhow::Result<Option<Vec<Configuration>>> {
    let Some(stored) = stored else { return Ok(None) };
    let entries: Vec<String> = if stored.is_empty() { setting.unwrap_or_else(|| vec![Configuration::DEV.name()]) } else { stored.to_vec() };
    if entries.iter().any(|entry| entry == FULL) {
        return Ok(None);
    }
    let mut configs = Vec::new();
    for entry in &entries {
        configs.push(parse_name(entry).ok_or_else(|| form_error(&format!("`{entry}` is not the name of a configuration")))?);
    }
    configs.sort_by(|a, b| (&a.features, a.release).cmp(&(&b.features, b.release)));
    configs.dedup();
    Ok(Some(configs))
}

#[cfg(test)]
mod tests {
    use super::*;

    // rule+ R85.2-1
    #[test]
    fn the_options_give_one_list_in_a_fixed_order() {
        let options = Options { release: true, features: vec!["b,a".into()], configurations: vec!["dev".into(), "full".into()], ..Options::default() };
        assert_eq!(options.list().unwrap(), Some(vec!["full".to_string(), "dev".into(), "release, features a,b".into()]));
        let other = Options { configurations: vec!["release, features a+b".into(), "full".into(), "dev".into()], release: false, ..Options::default() };
        assert_eq!(other.list().unwrap(), Some(vec!["full".to_string(), "dev".into(), "release, features a,b".into()]));
        assert_eq!(Options::default().list().unwrap(), None);
        // Each printed name reads back as its configuration.
        for name in ["dev", "release", "dev, no default features", "release, all features", "dev, features a,b", "release, no default features, features x", "dev, no default features (libraries and binaries only)"] {
            let config = parse_name(name).unwrap();
            assert_eq!(config.name(), name.trim_end_matches(Configuration::NARROW));
        }
        assert_eq!(option_of(&parse_name("release, features a,b").unwrap()), "--release --features a,b");
        assert_eq!(option_of(&Configuration::DEV), "--configuration dev");
        assert_eq!(command_options(&["release, all features".to_string()]), ["--configuration 'release, all features'"]);
    }

    // rule- R85.2-1
    #[test]
    fn a_wrong_name_or_a_wrong_pair_of_words_is_an_error() {
        for name in ["debug", "dev, features", "dev, all features, features a", "dev,, release", "release, all features, no default features", "dev, features a b"] {
            let options = Options { configurations: vec![name.into()], ..Options::default() };
            let error = options.list().unwrap_err().to_string();
            assert!(error.contains("is not the name of a configuration") && error.contains("`dev, features a,b`"), "{name}: {error}");
        }
        let options = Options { all_features: true, features: vec!["a".into()], ..Options::default() };
        assert!(options.list().unwrap_err().to_string().contains("`--all-features`"));
    }

    // rule+ R85.2-3
    #[test]
    fn the_setting_of_the_manifest_gives_the_default_list() {
        let text = "[package]\nname = \"a\"\n[package.metadata.semilla]\nconfigurations = [\"release, all features\", \"dev\"]\n";
        assert_eq!(setting(text).unwrap(), Some(vec!["dev".to_string(), "release, all features".into()]));
        let workspace = "[workspace.metadata.semilla]\nconfigurations = [\"full\"]\n[package.metadata.semilla]\nconfigurations = [\"dev\"]\n";
        assert_eq!(setting(workspace).unwrap(), Some(vec!["full".to_string()]));
        assert_eq!(resolve(Some(&[]), setting(text).unwrap()).unwrap().map(|list| list.iter().map(Configuration::name).collect::<Vec<_>>()), Some(vec!["dev".to_string(), "release, all features".into()]));
    }

    // rule- R85.2-3
    #[test]
    fn a_manifest_with_no_setting_or_a_bad_setting() {
        assert_eq!(setting("[package]\nname = \"a\"\n").unwrap(), None);
        assert!(setting("[package.metadata.semilla]\nconfigurations = [\"debug\"]\n").unwrap_err().to_string().contains("`debug`"));
        assert!(setting("[package.metadata.semilla]\nconfigurations = \"dev\"\n").is_err());
        // An option replaces the setting: a stored list that is not empty does not read it.
        let list = resolve(Some(&["release".to_string()]), Some(vec!["dev".into()])).unwrap().unwrap();
        assert_eq!(list.iter().map(Configuration::name).collect::<Vec<_>>(), ["release"]);
    }

    // rule+ R85.1-1
    #[test]
    fn a_step_with_no_option_has_dev_and_a_step_of_an_earlier_version_has_the_full_list() {
        let list = resolve(Some(&[]), None).unwrap().unwrap();
        assert_eq!(list, [Configuration::DEV]);
        assert_eq!(resolve(None, None).unwrap(), None);
    }

    // rule- R85.1-1
    #[test]
    fn the_entry_full_gives_the_full_list() {
        assert_eq!(resolve(Some(&["dev".to_string(), "full".into()]), None).unwrap(), None);
        assert_eq!(resolve(Some(&[]), Some(vec!["full".into()])).unwrap(), None);
    }
}
