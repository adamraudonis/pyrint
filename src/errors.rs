use std::fmt;
use std::path::PathBuf;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub code: String,
    pub message: String,
    pub file: PathBuf,
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub symbol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Convention,
    Refactor,
    Information,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Convention => write!(f, "convention"),
            Severity::Refactor => write!(f, "refactor"),
            Severity::Information => write!(f, "info"),
        }
    }
}

impl Issue {
    pub fn new(
        code: String,
        message: String,
        file: PathBuf,
        line: usize,
        column: usize,
        symbol: String,
    ) -> Self {
        let severity = match code.chars().next() {
            Some('E') => Severity::Error,
            Some('W') => Severity::Warning,
            Some('C') => Severity::Convention,
            Some('R') => Severity::Refactor,
            Some('I') => Severity::Information,
            _ => Severity::Error,
        };

        Self {
            code,
            message,
            file,
            line,
            column,
            severity,
            symbol,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ErrorCode {
    pub code: &'static str,
    pub symbol: &'static str,
    pub message_template: &'static str,
}

pub const E0001: ErrorCode = ErrorCode {
    code: "E0001",
    symbol: "syntax-error",
    message_template: "SyntaxError: {}",
};

pub const E0100: ErrorCode = ErrorCode {
    code: "E0100",
    symbol: "init-is-generator",
    message_template: "__init__ method is a generator",
};

pub const E0101: ErrorCode = ErrorCode {
    code: "E0101",
    symbol: "return-in-init",
    message_template: "Explicit return in __init__",
};

pub const E0102: ErrorCode = ErrorCode {
    code: "E0102",
    symbol: "function-redefined",
    message_template: "{}",
};

pub const E0103: ErrorCode = ErrorCode {
    code: "E0103",
    symbol: "not-in-loop",
    message_template: "{} not properly in loop",
};

pub const E0104: ErrorCode = ErrorCode {
    code: "E0104",
    symbol: "return-outside-function",
    message_template: "Return outside function",
};

pub const E0105: ErrorCode = ErrorCode {
    code: "E0105",
    symbol: "yield-outside-function",
    message_template: "Yield outside function",
};

pub const E0106: ErrorCode = ErrorCode {
    code: "E0106",
    symbol: "return-arg-in-generator",
    message_template: "Return with argument inside generator",
};

pub const E0107: ErrorCode = ErrorCode {
    code: "E0107",
    symbol: "nonexistent-operator",
    message_template: "Use of the non-existent {} operator",
};

pub const E0108: ErrorCode = ErrorCode {
    code: "E0108",
    symbol: "duplicate-argument-name",
    message_template: "Duplicate argument name {} in function definition",
};

pub const E0109: ErrorCode = ErrorCode {
    code: "E0109",
    symbol: "duplicate-key",
    message_template: "Duplicate key {} in dictionary",
};

pub const E0110: ErrorCode = ErrorCode {
    code: "E0110",
    symbol: "abstract-class-instantiated",
    message_template: "Abstract class {} instantiated",
};

pub const E0111: ErrorCode = ErrorCode {
    code: "E0111",
    symbol: "bad-reversed-sequence",
    message_template: "The first reversed() argument is not a sequence",
};

pub const E0112: ErrorCode = ErrorCode {
    code: "E0112",
    symbol: "too-many-star-expressions",
    message_template: "More than one starred expression in assignment",
};

pub const E0113: ErrorCode = ErrorCode {
    code: "E0113",
    symbol: "invalid-star-assignment-target",
    message_template: "Starred assignment target must be in a list or tuple",
};

pub const E0114: ErrorCode = ErrorCode {
    code: "E0114",
    symbol: "star-needs-assignment-target",
    message_template: "Can use starred expression only in assignment target",
};

pub const E0115: ErrorCode = ErrorCode {
    code: "E0115",
    symbol: "nonlocal-and-global",
    message_template: "Name '{}' is nonlocal and global",
};

pub const E0116: ErrorCode = ErrorCode {
    code: "E0116",
    symbol: "continue-not-in-loop",
    message_template: "'continue' not properly in loop",
};

pub const E0117: ErrorCode = ErrorCode {
    code: "E0117",
    symbol: "nonlocal-without-binding",
    message_template: "nonlocal name {} found without binding",
};

pub const E0118: ErrorCode = ErrorCode {
    code: "E0118",
    symbol: "used-prior-global-declaration",
    message_template: "Name {} is used prior to global declaration",
};

pub const E0119: ErrorCode = ErrorCode {
    code: "E0119",
    symbol: "misplaced-format-function",
    message_template: "format function is not called on str",
};

// New error codes for extended functionality
pub const E0202: ErrorCode = ErrorCode {
    code: "E0202",
    symbol: "method-hidden",
    message_template: "An attribute affected in {} line {} hide this method",
};

pub const E0203: ErrorCode = ErrorCode {
    code: "E0203",
    symbol: "access-member-before-definition",
    message_template: "Access to member '{}' before its definition line {}",
};

pub const E0211: ErrorCode = ErrorCode {
    code: "E0211",
    symbol: "no-method-argument",
    message_template: "Method '{}' has no argument",
};

pub const E0213: ErrorCode = ErrorCode {
    code: "E0213",
    symbol: "no-self-argument",
    message_template: "Method '{}' should have \"self\" as first argument",
};

pub const E0236: ErrorCode = ErrorCode {
    code: "E0236",
    symbol: "invalid-slots-object",
    message_template: "Invalid object '{}' in __slots__, must contain only non empty strings",
};

pub const E0237: ErrorCode = ErrorCode {
    code: "E0237",
    symbol: "assigning-non-slot",
    message_template: "Assigning to attribute '{}' not defined in class slots",
};

pub const E0238: ErrorCode = ErrorCode {
    code: "E0238",
    symbol: "invalid-slots",
    message_template: "Invalid __slots__ object",
};

pub const E0239: ErrorCode = ErrorCode {
    code: "E0239",
    symbol: "inherit-non-class",
    message_template: "Inheriting '{}', which is not a class.",
};

pub const E0241: ErrorCode = ErrorCode {
    code: "E0241",
    symbol: "duplicate-bases",
    message_template: "Duplicate bases for class '{}'",
};

pub const E0301: ErrorCode = ErrorCode {
    code: "E0301",
    symbol: "non-iterator-returned",
    message_template: "__iter__ returns non-iterator",
};

pub const E0302: ErrorCode = ErrorCode {
    code: "E0302",
    symbol: "unexpected-special-method-signature",
    message_template: "The special method '{}' expects {} param(s), {} was given",
};

pub const E0303: ErrorCode = ErrorCode {
    code: "E0303",
    symbol: "invalid-length-returned",
    message_template: "__len__ does not return non-negative integer",
};

pub const E0601: ErrorCode = ErrorCode {
    code: "E0601",
    symbol: "used-before-assignment",
    message_template: "Using variable '{}' before assignment",
};

pub const E0602: ErrorCode = ErrorCode {
    code: "E0602",
    symbol: "undefined-variable",
    message_template: "Undefined variable '{}'",
};

pub const E0606: ErrorCode = ErrorCode {
    code: "E0606",
    symbol: "possibly-used-before-assignment",
    message_template: "Possibly using variable '{}' before assignment",
};

pub const E0704: ErrorCode = ErrorCode {
    code: "E0704",
    symbol: "misplaced-bare-raise",
    message_template: "The raise statement is not inside an except clause",
};

pub const E0711: ErrorCode = ErrorCode {
    code: "E0711",
    symbol: "notimplemented-raised",
    message_template: "NotImplemented raised - should raise NotImplementedError",
};

pub const E1120: ErrorCode = ErrorCode {
    code: "E1120",
    symbol: "no-value-for-parameter",
    message_template: "No value for argument '{}' in function call",
};

pub const E1142: ErrorCode = ErrorCode {
    code: "E1142",
    symbol: "await-outside-async",
    message_template: "'await' outside async function",
};

pub const E1205: ErrorCode = ErrorCode {
    code: "E1205",
    symbol: "logging-too-many-args",
    message_template: "Too many arguments for logging format string",
};