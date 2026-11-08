pub fn f(v: &[u32]) -> u32 {
    v.iter().map(|x| x + 1).sum()
}
