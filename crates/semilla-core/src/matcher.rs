//! The one matcher of the old and the new code (design 72, the general rule G2, R72.1-1).
//!
//! Each pairing of an old thing with a new thing uses this function with one ladder of keys:
//!
//! 1. the identity that the compiler gives (a def path, the trait and the self type of an impl,
//!    the definition of a macro, the `cfg` condition of a twin). Two things of one identity
//!    always pair, as many as the two sides have: "removed" and "added" never show for two
//!    things with the same identity. In a class of more than one thing of one identity, the
//!    rungs below choose the partner;
//! 2. equal content (equal tokens): first the longest list of such pairs in their order, then
//!    each other such pair in source order;
//! 3. the best similarity, by a minimum-cost assignment over each candidate of the same group
//!    (the same kind and the same owner), with a threshold under which a thing stays unpaired.
//!    A pair by similarity that crosses the order of a pair of a stronger key (the pair implies a
//!    move) needs a margin (R72.1-2): it must be more similar than each partner of the pairs that
//!    it crosses. Else the two things stay unpaired;
//! 4. the source order, only to break a tie.
//!
//! Each kind gives its own measure of similarity and its own threshold. A pair keeps the key
//! that made it ([`Key`]): the reviewer sees a weak pair (R72.1-3).

use std::collections::BTreeMap;

/// The key that made a pair.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Key {
    /// The identity of the compiler (a class of one thing on each side).
    Identity,
    /// Equal content.
    Content,
    /// The best similarity, with its score.
    Similarity(f64),
    /// The source order: two things of one identity class with no better key.
    Order,
}

impl Key {
    /// True for a pair that no identity and no equal content made.
    pub fn weak(&self) -> bool {
        matches!(self, Key::Similarity(_) | Key::Order)
    }

    /// The words of the key for a line of the output: "a pair by similarity (0.83)".
    pub fn words(&self) -> String {
        match self {
            Key::Identity => "a pair by its identity".into(),
            Key::Content => "a pair by equal tokens".into(),
            Key::Similarity(score) => format!("a pair by similarity ({:.2})", score.min(1.0)),
            Key::Order => "a pair by the source order".into(),
        }
    }
}

/// One thing of one side. Its index in the list is its place in the source order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thing {
    /// Only two things of one group pair: the kind and the owner.
    pub group: String,
    /// The identity of the compiler (or of the fall-back text rule), or `None`.
    pub identity: Option<String>,
    /// The content key (equal tokens), or `None`.
    pub content: Option<String>,
}

/// One pair: the index of the old thing, of the new thing, and the key that made it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair {
    pub old: usize,
    pub new: usize,
    pub key: Key,
}

/// The measure of a kind.
pub struct Ladder<'a> {
    /// The similarity of an old thing and a new thing of one group (a kind can give a bonus over
    /// 1 for a local pair). A score under the threshold is no candidate.
    pub similarity: &'a dyn Fn(usize, usize) -> f64,
    pub threshold: f64,
    /// True if the pair (old, new) keeps the order of the stronger pair (old, new) of the second
    /// argument. `None`: the order of the two lists.
    pub in_order: Option<&'a dyn Fn((usize, usize), (usize, usize)) -> bool>,
}

/// The most things of one side of one assignment. Above it, the pairs come from the best score
/// first (ties by the source order): the cost of the assignment is the cube of the size.
pub const MOST_ASSIGNED: usize = 400;

/// The pairs of the old and the new things (R72.1-1). Each thing is in one pair at most.
pub fn pair(old: &[Thing], new: &[Thing], ladder: &Ladder) -> Vec<Pair> {
    pair_with(old, new, ladder, &[])
}

/// [`pair`] with the pairs that a kind already made by its own identity or content rule (they
/// count as the stronger pairs of the margin rule).
pub fn pair_with(old: &[Thing], new: &[Thing], ladder: &Ladder, fixed: &[Pair]) -> Vec<Pair> {
    let mut out: Vec<Pair> = fixed.to_vec();
    let mut taken = [vec![false; old.len()], vec![false; new.len()]];
    for pair in fixed {
        taken[0][pair.old] = true;
        taken[1][pair.new] = true;
    }
    let mut groups: BTreeMap<&str, [Vec<usize>; 2]> = BTreeMap::new();
    for (at, thing) in old.iter().enumerate().filter(|(at, _)| !taken[0][*at]) {
        groups.entry(thing.group.as_str()).or_default()[0].push(at);
    }
    for (at, thing) in new.iter().enumerate().filter(|(at, _)| !taken[1][*at]) {
        groups.entry(thing.group.as_str()).or_default()[1].push(at);
    }
    for (group, [olds, news]) in groups {
        if olds.is_empty() || news.is_empty() {
            continue;
        }
        let strong: Vec<Pair> = fixed.iter().copied().filter(|pair| old[pair.old].group == group).collect();
        out.extend(pair_group(old, new, &olds, &news, ladder, strong));
    }
    out.sort_by_key(|pair| (pair.old, pair.new));
    out
}

/// The ladder in one group.
fn pair_group(old: &[Thing], new: &[Thing], olds: &[usize], news: &[usize], ladder: &Ladder, mut strong: Vec<Pair>) -> Vec<Pair> {
    let mut out: Vec<Pair> = Vec::new();
    // Rung 1: the identity. A class of one thing on each side is a pair of the identity; a
    // larger class pairs as many things as it can, with the rungs below as the choice.
    let mut classes: BTreeMap<&str, [Vec<usize>; 2]> = BTreeMap::new();
    for &at in olds {
        if let Some(identity) = &old[at].identity {
            classes.entry(identity.as_str()).or_default()[0].push(at);
        }
    }
    for &at in news {
        if let Some(identity) = &new[at].identity {
            classes.entry(identity.as_str()).or_default()[1].push(at);
        }
    }
    let mut paired = [BTreeMap::new(), BTreeMap::new()];
    for [a, b] in classes.values() {
        match (a.as_slice(), b.as_slice()) {
            ([], _) | (_, []) => {}
            ([x], [y]) => out.push(Pair { old: *x, new: *y, key: Key::Identity }),
            _ => out.extend(pair_class(old, new, a, b, ladder)),
        }
    }
    for pair in &out {
        paired[0].insert(pair.old, pair.new);
        paired[1].insert(pair.new, pair.old);
    }
    strong.extend(out.iter().copied());
    // The pool: each thing that the identity did not pair.
    let pool_old: Vec<usize> = olds.iter().copied().filter(|at| !paired[0].contains_key(at)).collect();
    let pool_new: Vec<usize> = news.iter().copied().filter(|at| !paired[1].contains_key(at)).collect();
    let (content, rest_old, rest_new) = content_pairs(old, new, &pool_old, &pool_new);
    strong.extend(content.iter().copied());
    out.extend(content);
    out.extend(similarity_pairs(&rest_old, &rest_new, ladder, &strong));
    out
}

/// Rung 2 on two lists: the longest list of pairs with equal content in their order, then each
/// other old thing with the first new thing of equal content that is left. Also the things that
/// are left on each side.
fn content_pairs(old: &[Thing], new: &[Thing], olds: &[usize], news: &[usize]) -> (Vec<Pair>, Vec<usize>, Vec<usize>) {
    let same = |a: usize, b: usize| old[a].content.is_some() && old[a].content == new[b].content;
    let mut out = Vec::new();
    let mut used = [vec![false; olds.len()], vec![false; news.len()]];
    if olds.len().saturating_mul(news.len()) <= 4_000_000 {
        for (i, j) in lcs(olds.len(), news.len(), &|i, j| same(olds[i], news[j])) {
            used[0][i] = true;
            used[1][j] = true;
            out.push(Pair { old: olds[i], new: news[j], key: Key::Content });
        }
    }
    let mut by_content: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (j, &b) in news.iter().enumerate() {
        if let (false, Some(content)) = (used[1][j], &new[b].content) {
            by_content.entry(content.as_str()).or_default().push(j);
        }
    }
    for (i, &a) in olds.iter().enumerate() {
        let Some(content) = old[a].content.as_deref().filter(|_| !used[0][i]) else { continue };
        if let Some(list) = by_content.get_mut(content).filter(|list| !list.is_empty()) {
            let j = list.remove(0);
            used[0][i] = true;
            used[1][j] = true;
            out.push(Pair { old: a, new: news[j], key: Key::Content });
        }
    }
    let rest_old = olds.iter().enumerate().filter(|(i, _)| !used[0][*i]).map(|(_, at)| *at).collect();
    let rest_new = news.iter().enumerate().filter(|(j, _)| !used[1][*j]).map(|(_, at)| *at).collect();
    (out, rest_old, rest_new)
}

/// A class of more than one thing of one identity: each thing pairs while the other side has a
/// thing (rung 1). Equal content first, then the best similarity, then the source order.
fn pair_class(old: &[Thing], new: &[Thing], olds: &[usize], news: &[usize], ladder: &Ladder) -> Vec<Pair> {
    let (mut out, rest_old, rest_new) = content_pairs(old, new, olds, news);
    if rest_old.is_empty() || rest_new.is_empty() {
        return out;
    }
    // Each pair is a candidate: one more pair always costs less than a better score.
    let score = |a: usize, b: usize| (ladder.similarity)(a, b).max(0.0);
    let chosen = assign(&rest_old, &rest_new, &|a, b| Some(1.0 + score(a, b)));
    out.extend(chosen.into_iter().map(|(a, b)| {
        let found = score(a, b);
        Pair { old: a, new: b, key: if found >= ladder.threshold && found > 0.0 { Key::Similarity(found) } else { Key::Order } }
    }));
    out
}

/// Rung 3: the best similarity over the things that are left, with the threshold and the margin
/// of a pair that crosses a stronger pair (R72.1-2).
fn similarity_pairs(olds: &[usize], news: &[usize], ladder: &Ladder, strong: &[Pair]) -> Vec<Pair> {
    if olds.is_empty() || news.is_empty() {
        return Vec::new();
    }
    let score = |a: usize, b: usize| {
        let found = (ladder.similarity)(a, b);
        (found >= ladder.threshold && found > 0.0).then_some(found)
    };
    let chosen = assign(olds, news, &score);
    let in_order = |pair: (usize, usize), other: (usize, usize)| match ladder.in_order {
        Some(rule) => rule(pair, other),
        None => (other.0 < pair.0) == (other.1 < pair.1),
    };
    chosen
        .into_iter()
        .filter_map(|(a, b)| {
            let found = score(a, b)?;
            // The margin: each stronger pair that this pair crosses has partners that are less
            // similar to the two things of this pair.
            let crossed = strong.iter().filter(|other| !in_order((a, b), (other.old, other.new)));
            for other in crossed {
                if (ladder.similarity)(a, other.new) >= found || (ladder.similarity)(other.old, b) >= found {
                    return None;
                }
            }
            Some(Pair { old: a, new: b, key: Key::Similarity(found) })
        })
        .collect()
}

/// The assignment of the old things to the new things with the highest sum of scores (`None`:
/// no candidate). The source order breaks a tie: a pair at a nearer rank costs a little less.
/// The result has only candidate pairs. The best sum of a graph is the best sum of each of its
/// connected parts, so each part of the graph of the candidates gets its own assignment.
fn assign(olds: &[usize], news: &[usize], score: &dyn Fn(usize, usize) -> Option<f64>) -> Vec<(usize, usize)> {
    let (n, m) = (olds.len(), news.len());
    // A score is exact to 1e-6; the order term is at most 1e-9 · 400 · 2.
    let round = |value: f64| (value * 1e6).round() / 1e6;
    let tie = |i: usize, j: usize| 1e-9 * (i.abs_diff(j) as f64);
    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    for (i, &a) in olds.iter().enumerate() {
        for (j, &b) in news.iter().enumerate() {
            if let Some(found) = score(a, b) {
                edges.push((i, j, round(found)));
            }
        }
    }
    // The connected parts: a union of the rows (0..n) and the columns (n..n + m).
    let mut parent: Vec<usize> = (0..n + m).collect();
    fn root(parent: &mut [usize], mut at: usize) -> usize {
        while parent[at] != at {
            parent[at] = parent[parent[at]];
            at = parent[at];
        }
        at
    }
    for &(i, j, _) in &edges {
        let (x, y) = (root(&mut parent, i), root(&mut parent, n + j));
        if x != y {
            parent[x] = y;
        }
    }
    let mut parts: BTreeMap<usize, (Vec<usize>, Vec<usize>, Vec<(usize, usize, f64)>)> = BTreeMap::new();
    for &(i, j, found) in &edges {
        let key = root(&mut parent, i);
        let part = parts.entry(key).or_default();
        if !part.0.contains(&i) {
            part.0.push(i);
        }
        if !part.1.contains(&j) {
            part.1.push(j);
        }
        part.2.push((i, j, found));
    }
    let mut out = Vec::new();
    for (_, (mut rows, mut cols, part_edges)) in parts {
        rows.sort_unstable();
        cols.sort_unstable();
        if rows.len().max(cols.len()) > MOST_ASSIGNED {
            // The best score first, ties by the source order.
            let mut candidates: Vec<(f64, usize, usize)> = part_edges.iter().map(|&(i, j, found)| (found - tie(i, j), i, j)).collect();
            candidates.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(&y.1)).then(x.2.cmp(&y.2)));
            let mut used = [std::collections::BTreeSet::new(), std::collections::BTreeSet::new()];
            for (_, i, j) in candidates {
                if !used[0].contains(&i) && !used[1].contains(&j) {
                    used[0].insert(i);
                    used[1].insert(j);
                    out.push((olds[i], news[j]));
                }
            }
            continue;
        }
        let row_at: BTreeMap<usize, usize> = rows.iter().enumerate().map(|(at, i)| (*i, at)).collect();
        let col_at: BTreeMap<usize, usize> = cols.iter().enumerate().map(|(at, j)| (*j, at)).collect();
        let mut scores = vec![vec![None; cols.len()]; rows.len()];
        for &(i, j, found) in &part_edges {
            scores[row_at[&i]][col_at[&j]] = Some(found);
        }
        let transpose = rows.len() > cols.len();
        let (r_count, c_count) = if transpose { (cols.len(), rows.len()) } else { (rows.len(), cols.len()) };
        let cost: Vec<Vec<f64>> = (0..r_count)
            .map(|r| {
                (0..c_count)
                    .map(|c| {
                        let (x, y) = if transpose { (c, r) } else { (r, c) };
                        match scores[x][y] {
                            Some(found) => -found + tie(rows[x], cols[y]),
                            None => 0.0,
                        }
                    })
                    .collect()
            })
            .collect();
        for (r, c) in hungarian(&cost).into_iter().enumerate() {
            let (x, y) = if transpose { (c, r) } else { (r, c) };
            if scores[x][y].is_some() {
                out.push((olds[rows[x]], news[cols[y]]));
            }
        }
    }
    out.sort();
    out
}

/// The minimum-cost assignment of each row to one column (rows ≤ columns), by the Hungarian
/// method with potentials: O(rows² · columns). The result: the column of each row.
fn hungarian(cost: &[Vec<f64>]) -> Vec<usize> {
    let n = cost.len();
    if n == 0 {
        return Vec::new();
    }
    let m = cost[0].len();
    let mut u = vec![0.0f64; n + 1];
    let mut v = vec![0.0f64; m + 1];
    let mut p = vec![0usize; m + 1];
    let mut way = vec![0usize; m + 1];
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0usize;
        let mut minv = vec![f64::INFINITY; m + 1];
        let mut used = vec![false; m + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = f64::INFINITY;
            let mut j1 = 0usize;
            for j in 1..=m {
                if !used[j] {
                    let current = cost[i0 - 1][j - 1] - u[i0] - v[j];
                    if current < minv[j] {
                        minv[j] = current;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=m {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut out = vec![0usize; n];
    for j in 1..=m {
        if p[j] != 0 {
            out[p[j] - 1] = j - 1;
        }
    }
    out
}

/// The longest common subsequence of two lists of `n` and `m` things, as index pairs.
pub fn lcs(n: usize, m: usize, equal: &dyn Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if equal(i, j) { table[i + 1][j + 1] + 1 } else { table[i + 1][j].max(table[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if equal(i, j) {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// The similarity of two token lists: twice the length of their longest common subsequence over
/// the sum of their lengths (0 to 1). Two long lists use the shared count of each token.
pub fn token_similarity<T: Ord>(a: &[T], b: &[T]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = if a.len().saturating_mul(b.len()) > 4_000_000 {
        let mut counts: BTreeMap<&T, (usize, usize)> = BTreeMap::new();
        for token in a {
            counts.entry(token).or_default().0 += 1;
        }
        for token in b {
            counts.entry(token).or_default().1 += 1;
        }
        counts.values().map(|(x, y)| x.min(y)).sum::<usize>()
    } else {
        let mut previous = vec![0usize; b.len() + 1];
        for token in a {
            let mut current = vec![0usize; b.len() + 1];
            for (j, other) in b.iter().enumerate() {
                current[j + 1] = if token == other { previous[j] + 1 } else { current[j].max(previous[j + 1]) };
            }
            previous = current;
        }
        previous[b.len()]
    };
    2.0 * shared as f64 / (a.len() + b.len()) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn things(group: &str, list: &[(Option<&str>, Option<&str>)]) -> Vec<Thing> {
        list.iter().map(|(identity, content)| Thing { group: group.into(), identity: identity.map(String::from), content: content.map(String::from) }).collect()
    }

    fn texts(list: &[&str]) -> Vec<Thing> {
        list.iter().map(|text| Thing { group: "g".into(), identity: None, content: Some(text.to_string()) }).collect()
    }

    fn words(text: &str) -> Vec<&str> {
        text.split(' ').collect()
    }

    fn run(old: &[&str], new: &[&str], threshold: f64) -> Vec<(usize, usize, Key)> {
        let similarity = |a: usize, b: usize| token_similarity(&words(old[a]), &words(new[b]));
        let ladder = Ladder { similarity: &similarity, threshold, in_order: None };
        pair(&texts(old), &texts(new), &ladder).into_iter().map(|p| (p.old, p.new, p.key)).collect()
    }

    // rule+ R72.1-1
    #[test]
    fn the_identity_pairs_first_and_two_things_of_one_identity_always_pair() {
        // Two twins with a `cfg` condition in a new order: the identity pairs them.
        let old = things("k", &[(Some("cfg(a)"), Some("x 1")), (Some("cfg(b)"), Some("x 2"))]);
        let new = things("k", &[(Some("cfg(b)"), Some("x 2")), (Some("cfg(a)"), Some("x 3"))]);
        let zero = |_: usize, _: usize| 0.0;
        let ladder = Ladder { similarity: &zero, threshold: 0.5, in_order: None };
        let pairs = pair(&old, &new, &ladder);
        assert_eq!(pairs, vec![Pair { old: 0, new: 1, key: Key::Identity }, Pair { old: 1, new: 0, key: Key::Identity }]);
        // A class of two things of one identity with no similarity: they pair by the source order
        // (never a removed and an added thing).
        let old = things("k", &[(Some("same"), Some("a")), (Some("same"), Some("b"))]);
        let new = things("k", &[(Some("same"), Some("c")), (Some("same"), Some("d"))]);
        let pairs = pair(&old, &new, &ladder);
        assert_eq!(pairs, vec![Pair { old: 0, new: 0, key: Key::Order }, Pair { old: 1, new: 1, key: Key::Order }]);
        // Two groups never pair.
        let old = things("k", &[(None, Some("a"))]);
        let new = things("j", &[(None, Some("a"))]);
        assert!(pair(&old, &new, &ladder).is_empty());
    }

    // rule+ R72.1-1
    #[test]
    fn equal_content_comes_before_similarity_and_similarity_before_the_order() {
        // C-f of design 59.3: two changed calls and a new call at the start.
        let pairs = run(&["table ! ( A = 1 , )", "table ! ( B = 2 , )"], &["table ! ( Z = 9 , )", "table ! ( A = 5 , )", "table ! ( B = 6 , )"], 0.5);
        assert_eq!(pairs.iter().map(|(a, b, _)| (*a, *b)).collect::<Vec<_>>(), vec![(0, 1), (1, 2)]);
        assert!(pairs.iter().all(|(_, _, key)| matches!(key, Key::Similarity(_))));
        // Equal content wins over a more similar text in place.
        let pairs = run(&["a b c", "a b d"], &["a b d", "a b c"], 0.5);
        assert_eq!(pairs, vec![(0, 1, Key::Content), (1, 0, Key::Content)]);
        // Two equal scores: the source order breaks the tie.
        let pairs = run(&["x 1", "x 2"], &["x 3", "x 4"], 0.4);
        assert_eq!(pairs.iter().map(|(a, b, _)| (*a, *b)).collect::<Vec<_>>(), vec![(0, 0), (1, 1)]);
    }

    // rule- R72.1-1
    #[test]
    fn a_thing_under_the_threshold_stays_unpaired() {
        // A similarity of 0.25 is under the threshold of 0.5: no pair.
        let pairs = run(&["alpha beta gamma delta"], &["alpha one two three"], 0.5);
        assert!(pairs.is_empty(), "{pairs:?}");
        // The same two texts with a lower threshold pair.
        assert_eq!(run(&["alpha beta gamma delta"], &["alpha one two three"], 0.2).len(), 1);
        // The assignment takes the best sum, not the first best pair.
        let pairs = run(&["a b c d", "a b x y"], &["a b c y"], 0.5);
        assert_eq!(pairs.len(), 1);
    }

    // rule+ R72.1-2
    #[test]
    fn a_pair_that_crosses_a_stronger_pair_needs_a_margin() {
        // A table of rows: a row is removed in the middle and a row is added at the end. The
        // pair of the two rows crosses the pairs of the equal rows, and an equal row is as
        // similar: no pair.
        let old = ["push ( 1 , one )", "push ( 2 , two )", "push ( 3 , three )", "push ( 4 , four )"];
        let new = ["push ( 1 , one )", "push ( 2 , two )", "push ( 4 , four )", "push ( 6 , six )"];
        let pairs = run(&old, &new, 0.5);
        assert_eq!(pairs.iter().filter(|(_, _, key)| !matches!(key, Key::Content)).count(), 0, "{pairs:?}");
        // A row that changes in place keeps its pair: the pair crosses no stronger pair.
        let new = ["push ( 1 , one )", "push ( 2 , deux )", "push ( 3 , three )", "push ( 4 , four )"];
        let pairs = run(&old, &new, 0.5);
        assert!(pairs.contains(&(1, 1, Key::Similarity(token_similarity(&words(old[1]), &words(new[1]))))), "{pairs:?}");
    }

    // rule- R72.1-2
    #[test]
    fn a_moved_thing_with_a_clear_margin_keeps_its_pair() {
        let old = ["let p = a + 1 ;", "let q = b * 2 ;", "let r = c - 3 ;"];
        let new = ["let q = b * 2 ;", "let r = c - 3 ;", "let p = a + 2 ;"];
        let pairs = run(&old, &new, 0.5);
        assert!(pairs.iter().any(|(a, b, key)| (*a, *b) == (0, 2) && matches!(key, Key::Similarity(_))), "{pairs:?}");
    }

    #[test]
    fn the_assignment_finds_the_best_sum() {
        let scores = [[0.9, 0.6], [0.6, 0.0]];
        let olds = [0, 1];
        let news = [0, 1];
        let score = |a: usize, b: usize| (scores[a][b] > 0.0).then_some(scores[a][b]);
        let mut found = assign(&olds, &news, &score);
        found.sort();
        assert_eq!(found, vec![(0, 1), (1, 0)]);
    }

    // rule+ R72.1-3
    // rule- R72.1-3
    #[test]
    fn a_weak_key_says_so() {
        assert!(Key::Similarity(0.8).weak() && Key::Order.weak());
        assert!(!Key::Identity.weak() && !Key::Content.weak());
        assert_eq!(Key::Similarity(0.834).words(), "a pair by similarity (0.83)");
        assert_eq!(Key::Order.words(), "a pair by the source order");
    }
}
