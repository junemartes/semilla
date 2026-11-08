//! N1 content hashes: a Merkle tree over the call graph.
//!
//! The hash of a body covers its canonical text, with each call target that is a local function
//! replaced by the hash of that function. So the hash does not depend on the names of the local
//! functions, only on their content. Two bodies with the same hash have the same behavior: their
//! texts are the same up to calls of functions that have the same hash, and by induction those
//! functions have the same behavior.
//!
//! A cycle of the call graph (a strongly connected component) gets one hash. Inside the
//! component, each call of a member is replaced by the index of the member in a canonical order.
//! The order sorts the members by their text with all member calls masked. If two members have
//! the same masked text, the order is not unique, so the hash also includes the member names.
//! Then a rename inside the cycle is not absorbed, but two different cycles never get the same
//! hash.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::paths::replace_functions;

/// The input of the hash: the text and the local callees of a body, by name.
pub type HashInput = BTreeMap<String, (String, Vec<String>)>;

fn sha(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Returns the content hash of each body, as the token `#h:<hex>`.
pub fn content_hashes(bodies: &HashInput) -> BTreeMap<String, String> {
    let names: Vec<&String> = bodies.keys().collect();
    let index: BTreeMap<&str, usize> =
        names.iter().enumerate().map(|(i, name)| (name.as_str(), i)).collect();
    let edges: Vec<Vec<usize>> = names
        .iter()
        .map(|name| {
            let mut targets: Vec<usize> = bodies[*name]
                .1
                .iter()
                .filter_map(|callee| index.get(callee.as_str()).copied())
                .collect();
            targets.sort_unstable();
            targets.dedup();
            targets
        })
        .collect();

    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    for component in strongly_connected_components(&edges) {
        let members: Vec<&str> = component.iter().map(|&i| names[i].as_str()).collect();
        let external: Vec<(String, String)> = members
            .iter()
            .flat_map(|member| bodies[*member].1.iter())
            .filter_map(|callee| hashes.get(callee).map(|hash| (callee.clone(), hash.clone())))
            .collect();
        let cyclic = component.len() > 1 || edges[component[0]].contains(&component[0]);
        if !cyclic {
            let name = members[0];
            let text = replace_functions(&bodies[name].0, &external);
            hashes.insert(name.to_string(), format!("#h:{}", sha(&format!("fn\n{text}"))));
            continue;
        }

        // Mask the member calls, and sort the members by their masked text.
        let masks: Vec<(String, String)> =
            members.iter().map(|m| (m.to_string(), "@scc".to_string())).collect();
        let mut keyed: Vec<(String, &str)> = members
            .iter()
            .map(|member| {
                let mut pairs = external.clone();
                pairs.extend(masks.iter().cloned());
                (sha(&replace_functions(&bodies[*member].0, &pairs)), *member)
            })
            .collect();
        keyed.sort();
        let unique = keyed.windows(2).all(|pair| pair[0].0 != pair[1].0);
        if unique {
            let positions: Vec<(String, String)> = keyed
                .iter()
                .enumerate()
                .map(|(i, (_, member))| (member.to_string(), format!("@scc#{i}")))
                .collect();
            let mut pairs = external.clone();
            pairs.extend(positions);
            let texts: Vec<String> = keyed
                .iter()
                .map(|(_, member)| replace_functions(&bodies[*member].0, &pairs))
                .collect();
            let component_hash = sha(&format!("scc\n{}", texts.join("\n---\n")));
            for (i, (_, member)) in keyed.iter().enumerate() {
                hashes.insert(
                    member.to_string(),
                    format!("#h:{}", sha(&format!("{component_hash}#{i}"))),
                );
            }
        } else {
            // The order is not unique: include the names.
            let texts: Vec<String> = members
                .iter()
                .map(|member| {
                    format!("{member}\n{}", replace_functions(&bodies[*member].0, &external))
                })
                .collect();
            let component_hash = sha(&format!("scc-named\n{}", texts.join("\n---\n")));
            for member in &members {
                hashes.insert(
                    member.to_string(),
                    format!("#h:{}", sha(&format!("{component_hash}#{member}"))),
                );
            }
        }
    }
    hashes
}

/// Returns the strongly connected components of a graph, callees first (reverse topological
/// order). Each component lists its nodes in increasing order. This is Tarjan's algorithm,
/// without recursion.
pub fn strongly_connected_components(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let count = edges.len();
    let mut index = vec![usize::MAX; count];
    let mut low = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut stack = Vec::new();
    let mut next = 0;
    let mut components = Vec::new();
    for start in 0..count {
        if index[start] != usize::MAX {
            continue;
        }
        let mut work = vec![(start, 0usize)];
        while let Some((node, edge)) = work.pop() {
            if edge == 0 {
                index[node] = next;
                low[node] = next;
                next += 1;
                stack.push(node);
                on_stack[node] = true;
            }
            if let Some(&target) = edges[node].get(edge) {
                work.push((node, edge + 1));
                if index[target] == usize::MAX {
                    work.push((target, 0));
                } else if on_stack[target] {
                    low[node] = low[node].min(index[target]);
                }
                continue;
            }
            if low[node] == index[node] {
                let mut component = Vec::new();
                loop {
                    let member = stack.pop().expect("the stack holds the component");
                    on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                component.sort_unstable();
                components.push(component);
            }
            if let Some(&(parent, _)) = work.last() {
                low[parent] = low[parent].min(low[node]);
            }
        }
    }
    components
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(bodies: &[(&str, &str, &[&str])]) -> HashInput {
        bodies
            .iter()
            .map(|(name, text, callees)| {
                (
                    name.to_string(),
                    (text.to_string(), callees.iter().map(|c| c.to_string()).collect()),
                )
            })
            .collect()
    }

    #[test]
    fn components_come_callees_first() {
        // 0 -> 1 -> 2 -> 1, 3 alone.
        let edges = vec![vec![1], vec![2], vec![1], vec![]];
        let components = strongly_connected_components(&edges);
        assert_eq!(components, vec![vec![1, 2], vec![0], vec![3]]);
    }

    #[test]
    fn self_loop_is_its_own_component() {
        let edges = vec![vec![0], vec![0]];
        assert_eq!(strongly_connected_components(&edges), vec![vec![0], vec![1]]);
    }

    #[test]
    fn renamed_callee_gives_the_same_caller_hash() {
        let before =
            input(&[("k::helper", "x + 1", &[]), ("k::f", "call {k::helper}", &["k::helper"])]);
        let after =
            input(&[("k::add_one", "x + 1", &[]), ("k::f", "call {k::add_one}", &["k::add_one"])]);
        let (b, a) = (content_hashes(&before), content_hashes(&after));
        assert_eq!(b["k::helper"], a["k::add_one"]);
        assert_eq!(b["k::f"], a["k::f"]);
    }

    #[test]
    fn changed_callee_changes_the_caller_hash() {
        let before =
            input(&[("k::helper", "x + 1", &[]), ("k::f", "call {k::helper}", &["k::helper"])]);
        let after =
            input(&[("k::helper", "x + 2", &[]), ("k::f", "call {k::helper}", &["k::helper"])]);
        assert_ne!(content_hashes(&before)["k::f"], content_hashes(&after)["k::f"]);
    }

    #[test]
    fn cycle_hash_ignores_names_but_keeps_structure() {
        // even/odd recursion, renamed.
        let before = input(&[
            ("k::even", "if n == 0 { true } else { {k::odd}(n - 1) }", &["k::odd"]),
            ("k::odd", "if n == 0 { false } else { {k::even}(n - 1) }", &["k::even"]),
        ]);
        let after = input(&[
            ("k::is_even", "if n == 0 { true } else { {k::is_odd}(n - 1) }", &["k::is_odd"]),
            ("k::is_odd", "if n == 0 { false } else { {k::is_even}(n - 1) }", &["k::is_even"]),
        ]);
        let (b, a) = (content_hashes(&before), content_hashes(&after));
        assert_eq!(b["k::even"], a["k::is_even"]);
        assert_eq!(b["k::odd"], a["k::is_odd"]);
        assert_ne!(b["k::even"], b["k::odd"]);
    }

    #[test]
    fn cycle_hash_keeps_which_member_is_called() {
        // The masked texts are the same, but `a` calls the members in another order.
        let before = input(&[
            ("k::a", "A {k::b} {k::a}", &["k::a", "k::b"]),
            ("k::b", "B {k::a}", &["k::a"]),
        ]);
        let after = input(&[
            ("k::a", "A {k::a} {k::b}", &["k::a", "k::b"]),
            ("k::b", "B {k::a}", &["k::a"]),
        ]);
        assert_ne!(content_hashes(&before)["k::a"], content_hashes(&after)["k::a"]);
    }

    #[test]
    fn equal_masked_members_keep_their_names() {
        // Two members with the same masked text: the hash includes the names.
        let first =
            input(&[("k::a", "call {k::b}", &["k::b"]), ("k::b", "call {k::a}", &["k::a"])]);
        let second =
            input(&[("k::x", "call {k::y}", &["k::y"]), ("k::y", "call {k::x}", &["k::x"])]);
        assert_ne!(content_hashes(&first)["k::a"], content_hashes(&second)["k::x"]);
    }
}
