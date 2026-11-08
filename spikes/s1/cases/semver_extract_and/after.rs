fn has_pre(patch: Option<u64>, text: &str) -> bool {
    patch.is_some() && text.starts_with('-')
}

pub fn pre(patch: Option<u64>, text: &str) -> &str {
    if has_pre(patch, text) { &text[1..] } else { text }
}
