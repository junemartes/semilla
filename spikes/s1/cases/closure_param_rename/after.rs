pub fn f(v: &[u32]) -> u32 {
    v.iter().map(|n| n + 1).sum()
}
