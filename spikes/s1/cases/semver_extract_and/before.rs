pub fn pre(patch: Option<u64>, text: &str) -> &str {
    if patch.is_some() && text.starts_with('-') { &text[1..] } else { text }
}
