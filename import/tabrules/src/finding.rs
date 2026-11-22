use std::fmt;

/// One violation. Line 0 means the finding is about the whole file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub rule: &'static str,
    pub message: String,
    /// The project's citation for the rule, from `cite` in its config.
    pub cite: Option<String>,
}

impl Finding {
    pub fn new(path: &str, line: usize, rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            line,
            rule,
            message: message.into(),
            cite: None,
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path)?;
        if self.line != 0 {
            write!(f, ":{}", self.line)?;
        }
        write!(f, ": {}: {}", self.rule, self.message)?;
        if let Some(cite) = &self.cite {
            write!(f, " [{cite}]")?;
        }
        Ok(())
    }
}
