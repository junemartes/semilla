pub fn f(x: Option<u32>) -> Option<u32> {
    let v = match x {
        Some(v) => v,
        None => return None,
    };
    Some(v + 1)
}
