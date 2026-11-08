pub fn f(v: &[u32], i: usize, j: usize) -> u32 {
    let b = v[j];
    let a = v[i];
    a ^ b
}
