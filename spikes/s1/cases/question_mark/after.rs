pub fn f(x: Option<u32>) -> Option<u32> {
    let v = x?;
    Some(v + 1)
}
