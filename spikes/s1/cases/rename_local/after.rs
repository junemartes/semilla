pub fn total(prices: &[u32]) -> u32 {
    let mut acc = 0;
    for price in prices {
        acc += price;
    }
    acc
}
