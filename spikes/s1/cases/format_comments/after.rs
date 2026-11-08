/// Adds all prices.
pub fn total(prices: &[u32]) -> u32 {
    // Start at zero.
    let mut sum = 0;
    for p in prices { sum += p; }   // one line
    sum
}
