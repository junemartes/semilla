pub fn total(prices: &[u32]) -> u32 {
    let mut sum = 0;
    for p in prices {
        sum += p;
    }
    sum
}
