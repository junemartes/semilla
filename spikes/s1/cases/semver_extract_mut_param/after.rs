pub struct Error {
    pub kind: u32,
}

fn wildcard(input: &str) -> Option<(char, &str)> {
    if let Some(rest) = input.strip_prefix('*') {
        Some(('*', rest))
    } else if let Some(rest) = input.strip_prefix('x') {
        Some(('x', rest))
    } else {
        None
    }
}

fn mark_lone_wildcard(input: &str, error: &mut Error) {
    if let Some((ch, mut rest)) = wildcard(input) {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() || rest.starts_with(',') {
            error.kind = ch as u32;
        }
    }
}

pub fn fix_up(input: &str, mut error: Error) -> Error {
    mark_lone_wildcard(input, &mut error);
    error
}
