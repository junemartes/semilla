pub fn parts(patch: Option<u64>, text: &str) -> (u32, u32) {
    let pre = if patch.is_some() && text.starts_with('-') { 1 } else { 2 };
    let build = if patch.is_some() && text.starts_with('+') { 4 } else { 8 };
    (pre, build)
}
