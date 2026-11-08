pub fn get(v: &[u32], i: usize) -> Option<u32> {
    if i > v.len() {
        return None;
    }
    Some(v[i])
}
