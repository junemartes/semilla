#[inline(never)]
fn bump(mut x: u32) -> u32 {
    x += 1;
    x
}
pub fn f(a: u32) -> u32 {
    let y = bump(a);
    y ^ a
}
