//! The error every port returns. The message says what failed and, when it helps, what to do next:
//! MCP tools hand it to the client as is and the UI shows it in a toast.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A failure with a message written for whoever reads the tool reply or toast.
    #[error("{0}")]
    Message(String),

    #[error("{action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The feature has no implementation on this platform.
    #[error("{0} is not supported on this platform")]
    Unsupported(&'static str),
}

impl Error {
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    pub fn io(action: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { action, path: path.into(), source }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
