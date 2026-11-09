use std::fmt;

/// A position in the source, 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Syntax,
    Runtime,
    Snapshot,
}

/// One entry of a runtime stack trace, innermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFrame {
    pub function: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub span: Option<Span>,
    pub trace: Vec<TraceFrame>,
}

impl Error {
    pub fn syntax(message: impl Into<String>, span: Span) -> Self {
        Error {
            kind: ErrorKind::Syntax,
            message: message.into(),
            span: Some(span),
            trace: Vec::new(),
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Error {
            kind: ErrorKind::Runtime,
            message: message.into(),
            span: None,
            trace: Vec::new(),
        }
    }

    pub fn snapshot(message: impl Into<String>) -> Self {
        Error {
            kind: ErrorKind::Snapshot,
            message: message.into(),
            span: None,
            trace: Vec::new(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::Syntax => write!(f, "syntax error")?,
            ErrorKind::Runtime => write!(f, "runtime error")?,
            ErrorKind::Snapshot => write!(f, "bad checkpoint")?,
        }
        if let Some(span) = self.span {
            write!(f, " at {}:{}", span.line, span.col)?;
        }
        write!(f, ": {}", self.message)?;
        for frame in &self.trace {
            write!(f, "\n    in {} (line {})", frame.function, frame.line)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}
