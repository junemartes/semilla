pub fn f(a: u32) -> u32 {
    let mut x = a;
    let t = x;
    x = 5;
    t ^ x
}
