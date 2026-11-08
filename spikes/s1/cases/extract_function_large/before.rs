pub fn rank(items: &[(u32, u32, u32)]) -> u32 {
    let mut best = 0;
    for &(a, b, c) in items {
        let mut s = a * 3 + b * 2 + c;
        if s > 100 { s -= 100; } else { s += a ^ b; }
        if s % 2 == 0 { s /= 2; } else { s = s * 3 + 1; }
        if c > a && c > b { s += c - a; }
        if s > best { best = s; }
    }
    best
}
