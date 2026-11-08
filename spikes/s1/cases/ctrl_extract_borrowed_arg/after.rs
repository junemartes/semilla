#[inline(never)]
fn has_pre(patch: Option<u64>, text: &str) -> bool {
    patch.is_some() && text.starts_with('-')
}

pub fn parts(mut patch: Option<u64>, text: &str) -> (u32, u32) {
    patch = None;
    let pre = if has_pre(patch, text) { 1 } else { 2 };
    let build = if patch.is_some() && text.starts_with('+') { 4 } else { 8 };
    (pre, build)
}
