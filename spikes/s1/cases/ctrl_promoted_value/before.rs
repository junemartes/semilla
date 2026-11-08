pub fn pair<T: Copy>(x: T) -> (T, &'static [u32]) {
    (x, &[1, 2, 3])
}
