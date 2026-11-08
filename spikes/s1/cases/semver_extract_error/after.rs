#[derive(Clone, Copy)]
pub enum Position {
    Major,
    Pre,
}

pub enum ErrorKind {
    EmptySegment(Position),
    Other,
}

pub struct Error {
    pub kind: ErrorKind,
}

impl Error {
    pub fn new(kind: ErrorKind) -> Error {
        Error { kind }
    }
}

fn empty_segment(pos: Position) -> Error {
    Error::new(ErrorKind::EmptySegment(pos))
}

pub fn check(text: &str, pos: Position) -> Result<usize, Error> {
    if text.is_empty() {
        return Err(empty_segment(pos));
    }
    Ok(text.len())
}
