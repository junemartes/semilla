pub fn f(c: bool) -> (u32, u32) {
    let mut a = 0;
    let mut b = 0;
    let r = if c { &mut a } else { &mut b };
    *r = 1;
    (a, b)
}
