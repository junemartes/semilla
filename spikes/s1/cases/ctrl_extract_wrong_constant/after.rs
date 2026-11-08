fn score(a: u32, b: u32, c: u32) -> u32 {
    let mut s = a * 3 + b * 2 + c;
    if s > 100 { s -= 101; } else { s += a ^ b; }
    if s % 2 == 0 { s /= 2; } else { s = s * 3 + 1; }
    if c > a && c > b { s += c - a; }
    s
}
pub fn rank(items: &[(u32, u32, u32)]) -> u32 {
    let mut best = 0;
    for &(a, b, c) in items {
        let s = score(a, b, c);
        if s > best { best = s; }
    }
    best
}
