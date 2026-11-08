pub struct Noisy(pub u32);
impl Drop for Noisy {
    fn drop(&mut self) {
        println!("{}", self.0);
    }
}
pub fn f() {
    let _a = Noisy(1);
    let _b = Noisy(2);
}
