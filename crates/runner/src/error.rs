use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no free port in {first}..={last}")]
    NoFreePort { first: u16, last: u16 },
    #[error("{stack} cannot run on {platform}")]
    Mismatch {
        stack: &'static str,
        platform: &'static str,
    },
    #[error("{0} has several schemes; pick one")]
    SchemeRequired(String),
    #[error("{0} has no scheme")]
    NoScheme(String),
    #[error("build output not found: {}", .0.display())]
    ArtifactMissing(PathBuf),
    #[error("cannot parse {what}: {detail}")]
    Parse { what: &'static str, detail: String },
    #[error("`{command}` failed: {stderr}")]
    Command { command: String, stderr: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
