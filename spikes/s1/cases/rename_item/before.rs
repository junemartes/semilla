fn helper(x: u32) -> u32 {
    x + 1
}
pub fn f(x: u32) -> u32 {
    helper(x) * 2
}
