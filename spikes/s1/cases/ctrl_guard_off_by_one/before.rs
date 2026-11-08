pub fn get(v: &[u32], i: usize) -> Option<u32> {
    if i < v.len() { Some(v[i]) } else { None }
}
