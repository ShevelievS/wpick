use thiserror::Error;

#[derive(Debug, Error)]
pub enum WpickError {
    /// Wraps std::io::Error (file I/O, socket I/O, etc.)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Wraps rusqlite::Error (SQLite operations)
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Wraps serde_json::Error (IPC serialization / project.json parsing)
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Wraps toml::de::Error (config file deserialization)
    #[error("Config TOML parse error: {0}")]
    ConfigToml(#[from] toml::de::Error),

    /// Generic config error (missing dirs, serialization failures, etc.)
    #[error("Config error: {0}")]
    Config(String),

    /// Wallpaper with the given id was not found in the cache
    #[error("Wallpaper not found: id={id}")]
    WallpaperNotFound { id: u64 },

    /// VDF file could not be parsed; caller decides whether to skip or abort
    #[error("VDF parse error in {path}: {reason}")]
    VdfParse { path: String, reason: String },

    /// IPC connection was closed cleanly by the remote side
    #[error("IPC connection closed")]
    IpcClosed,

    /// Received a message that violates the IPC protocol
    #[error("IPC protocol error: {0}")]
    IpcProtocol(String),
}

pub type Result<T> = std::result::Result<T, WpickError>;
