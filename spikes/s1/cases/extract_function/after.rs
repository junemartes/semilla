fn line(p: u32, tax: u32) -> u32 {
    p * tax
}
pub fn total(prices: &[u32], tax: u32) -> u32 {
    let mut sum = 0;
    for p in prices {
        sum += line(*p, tax);
    }
    sum
}
