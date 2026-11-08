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

fn ends_comparator(rest: &str) -> bool {
    rest.is_empty() || rest.starts_with(',')
}

pub fn fix_up(input: &str, mut error: Error) -> Error {
    if let Some((ch, mut rest)) = wildcard(input) {
        rest = rest.trim_start_matches(' ');
        if ends_comparator(rest) {
            error.kind = ch as u32;
        }
    }
    error
}
