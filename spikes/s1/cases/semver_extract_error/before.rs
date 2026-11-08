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

pub fn check(text: &str, pos: Position) -> Result<usize, Error> {
    if text.is_empty() {
        return Err(Error::new(ErrorKind::EmptySegment(pos)));
    }
    Ok(text.len())
}
