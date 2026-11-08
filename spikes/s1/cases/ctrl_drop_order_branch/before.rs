pub struct Noisy(pub u32);
impl Drop for Noisy {
    fn drop(&mut self) {
        println!("{}", self.0);
    }
}
pub fn f(c: bool) {
    if c {
        let _a = Noisy(1);
        let _b = Noisy(2);
    } else {
        let _c = Noisy(3);
    }
}
