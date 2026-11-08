pub fn f() -> u32 {
    let mut x = 0u32;
    let mut y = 0u32;
    std::hint::black_box(&mut x);
    std::hint::black_box(&mut y);
    x
}
