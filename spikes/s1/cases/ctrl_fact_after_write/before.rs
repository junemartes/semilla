pub fn f(p: &mut u32) -> u32 {
    if *p > 0 {
        *p = 0;
        if *p > 0 { 1 } else { 2 }
    } else {
        3
    }
}
