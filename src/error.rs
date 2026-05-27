use thiserror::Error;

#[derive(Error, Debug)]
pub enum HarnessError {
    #[error("API error: {0}")]
    Api(String),
    #[error("Config error: {0}")]
    Config(String),
    #[error("Tool error: {0}")]
    Tool(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Hook error: {0}")]
    Hook(String),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Session reset")]
    SessionReset,
}

pub type Result<T> = std::result::Result<T, HarnessError>;
