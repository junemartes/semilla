pub fn pair<T: Copy>(x: T) -> (T, &'static [u32]) {
    (x, &[1, 2, 3])
}

pub fn probe(x: u32) -> u32 {
    x + 1
}
