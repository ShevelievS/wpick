use serde::{Deserialize, Serialize};

use crate::config::FitMode;
use crate::error::{Result, WpickError};
use crate::model::WallpaperInfo;

// 64 KiB — the largest valid command is a few hundred bytes; this caps OOM risk
// from a malicious or buggy client on the local Unix socket.
const MAX_CMD_BYTES: usize = 64 * 1024;
// 16 MiB — WallpaperList can carry thousands of entries.
const MAX_RESP_BYTES: usize = 16 * 1024 * 1024;

// ─── Protocol enums ───────────────────────────────────────────────────────────

/// Commands sent from wpick-tui to wpick-daemon over the Unix socket.
/// Wire format: `{"type":"List"}`, `{"type":"Set","id":42}`, etc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientCommand {
    List,
    Scan,
    /// Apply wallpaper `id` to one monitor (by wl_output name) or all monitors
    /// when `monitor` is absent.  Old clients that omit `monitor` continue to work.
    Set {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        monitor: Option<String>,
    },
    Volume { level: f32 },
    Mute,
    /// Query current volume and mute state without changing anything.
    Status,
    Info   { id: u64 },
    /// Return the names of all currently connected wl_outputs.
    ListOutputs,
    /// Set the fit/scale mode for a specific monitor (or all if `monitor` is absent).
    SetFit {
        fit: FitMode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        monitor: Option<String>,
    },
    /// Start the wallpaper timer: cycle through `ids` every `interval_secs` seconds.
    /// `shuffle = true` randomises the order each cycle.
    /// Replaces any running timer.
    SetTimer {
        ids:          Vec<u64>,
        interval_secs: u64,
        shuffle:      bool,
    },
    /// Stop the running timer (no-op if no timer is running).
    StopTimer,
    /// Query the current timer state without changing it.
    GetTimerState,
    /// Record that wallpaper `id` was played (for Frequent tracking).
    RecordPlay { id: u64 },
    /// Return up to `limit` wallpapers ordered by play count descending.
    GetFrequent { limit: usize },
    Kill,
    /// Assign wallpaper `id` to workspace `workspace` (Hyprland name like "1", "2",
    /// "special:magic").  Pass `id = 0` to clear the mapping for that workspace.
    SetWorkspaceWallpaper { workspace: String, id: u64 },
    /// Return the current workspace → wallpaper id mapping.
    GetWorkspaceMap,
}

/// Responses sent from wpick-daemon back to wpick-tui.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonResponse {
    Ok,
    Error         { message: String },
    WallpaperList { items: Vec<WallpaperInfo> },
    WallpaperInfo { item: WallpaperInfo },
    /// Returned by Volume, Mute, and Status — carries the authoritative runtime state.
    /// `current_id` is the active wallpaper Workshop ID, or None when nothing is playing.
    /// `#[serde(default)]` + `skip_serializing_if` keeps the field absent on the
    /// wire when None, so v0.1 clients that don't know the field see nothing.
    VolumeState {
        volume:     f32,
        muted:      bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_id: Option<u64>,
    },
    /// Streamed zero or more times before the final `WallpaperList` in response to `Scan`.
    /// `done` wallpapers have been processed out of `total` discovered.
    ScanProgress { done: usize, total: usize },
    /// Response to `ListOutputs` — the wl_output names currently connected.
    OutputList {
        names: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resolutions: Vec<(u32, u32)>,
    },
    /// Current timer state, returned by `SetTimer`, `StopTimer`, and `GetTimerState`.
    TimerState {
        active:         bool,
        interval_secs:  u64,
        /// Seconds until the next wallpaper change (0 when inactive).
        remaining_secs: u64,
        /// Wallpaper IDs in current rotation (empty when inactive).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ids: Vec<u64>,
    },
    /// Response to `GetFrequent` — most-played wallpapers in descending play-count order.
    FrequentList { items: Vec<WallpaperInfo> },
    /// Returned by `GetWorkspaceMap` and `SetWorkspaceWallpaper`.
    WorkspaceMap {
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        map: std::collections::HashMap<String, u64>,
    },
}

// ─── Send / Receive helpers ───────────────────────────────────────────────────

/// Serialize `cmd` to a newline-terminated JSON line and write it to `writer`.
/// Flushes after write (required — see ERRORS_TO_AVOID E-18).
pub async fn send_command<W>(writer: &mut W, cmd: &ClientCommand) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let mut json = serde_json::to_string(cmd)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one newline-terminated JSON line and deserialize it as `ClientCommand`.
/// Returns `WpickError::IpcClosed` on EOF (see ERRORS_TO_AVOID E-20).
///
/// Uses `take(MAX_CMD_BYTES + 1)` so the internal buffer never grows beyond
/// the limit regardless of what the peer sends — prevents memory exhaustion DoS.
pub async fn recv_command<R>(reader: &mut R) -> Result<ClientCommand>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut line = String::new();
    let n = {
        let mut limited = (&mut *reader).take((MAX_CMD_BYTES as u64) + 1);
        limited.read_line(&mut line).await?
    };
    if n == 0 {
        return Err(WpickError::IpcClosed);
    }
    if n > MAX_CMD_BYTES {
        return Err(WpickError::IpcProtocol(format!(
            "command too large ({n} bytes, max {MAX_CMD_BYTES})"
        )));
    }
    Ok(serde_json::from_str(line.trim())?)
}

/// Serialize `resp` to a newline-terminated JSON line and write it to `writer`.
/// Flushes after write.
pub async fn send_response<W>(writer: &mut W, resp: &DaemonResponse) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let mut json = serde_json::to_string(resp)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one newline-terminated JSON line and deserialize it as `DaemonResponse`.
/// Returns `WpickError::IpcClosed` on EOF.
///
/// Uses `take(MAX_RESP_BYTES + 1)` to cap allocation before the size check.
pub async fn recv_response<R>(reader: &mut R) -> Result<DaemonResponse>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut line = String::new();
    let n = {
        let mut limited = (&mut *reader).take((MAX_RESP_BYTES as u64) + 1);
        limited.read_line(&mut line).await?
    };
    if n == 0 {
        return Err(WpickError::IpcClosed);
    }
    if n > MAX_RESP_BYTES {
        return Err(WpickError::IpcProtocol(format!(
            "response too large ({n} bytes, max {MAX_RESP_BYTES})"
        )));
    }
    Ok(serde_json::from_str(line.trim())?)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Serialization round-trips ────────────────────────────────────────────

    #[test]
    fn test_all_client_commands_roundtrip() -> crate::error::Result<()> {
        let commands = vec![
            ClientCommand::List,
            ClientCommand::Scan,
            ClientCommand::Set    { id: 42, monitor: None },
            ClientCommand::Set    { id: 7,  monitor: Some("DP-1".into()) },
            ClientCommand::Volume { level: 0.5 },
            ClientCommand::Mute,
            ClientCommand::Status,
            ClientCommand::Info   { id: 99 },
            ClientCommand::ListOutputs,
            ClientCommand::Kill,
            ClientCommand::SetWorkspaceWallpaper { workspace: "1".into(), id: 42 },
            ClientCommand::SetWorkspaceWallpaper { workspace: "special:magic".into(), id: 0 },
            ClientCommand::GetWorkspaceMap,
        ];

        for cmd in &commands {
            let json = serde_json::to_string(cmd)?;
            let back: ClientCommand = serde_json::from_str(&json)?;
            assert_eq!(cmd, &back, "round-trip failed for {:?}", cmd);
        }

        // Spot-check wire format
        assert_eq!(serde_json::to_string(&ClientCommand::List)?,   r#"{"type":"List"}"#);
        assert_eq!(serde_json::to_string(&ClientCommand::Scan)?,   r#"{"type":"Scan"}"#);
        // monitor=None must be omitted for backward compat with old daemons
        assert_eq!(serde_json::to_string(&ClientCommand::Set { id: 42, monitor: None })?,
            r#"{"type":"Set","id":42}"#);
        assert_eq!(serde_json::to_string(&ClientCommand::Set { id: 7, monitor: Some("DP-1".into()) })?,
            r#"{"type":"Set","id":7,"monitor":"DP-1"}"#);
        assert_eq!(serde_json::to_string(&ClientCommand::Volume { level: 0.5 })?, r#"{"type":"Volume","level":0.5}"#);
        // Forward compat: old {"type":"Set","id":42} (no monitor field) must deserialize correctly
        let old: ClientCommand = serde_json::from_str(r#"{"type":"Set","id":42}"#)?;
        assert_eq!(old, ClientCommand::Set { id: 42, monitor: None });

        Ok(())
    }

    #[test]
    fn test_all_daemon_responses_roundtrip() -> crate::error::Result<()> {
        use crate::model::{WallpaperInfo, WallpaperType};

        let sample_info = WallpaperInfo {
            id:              1,
            title:           "Sample".into(),
            wallpaper_type:  WallpaperType::Video,
            file_path:       "/tmp/v.mp4".into(),
            preview_path:    None,
            has_audio:       false,
            file_size_bytes: 0,
            width:           0,
            height:          0,
            source:          crate::model::WallpaperSource::Workshop,
        };

        // Every variant must be present — if a new variant is added without
        // updating this list, the enum coverage gap should be caught in review.
        let responses: Vec<DaemonResponse> = vec![
            DaemonResponse::Ok,
            DaemonResponse::Error { message: "oops".into() },
            DaemonResponse::WallpaperList { items: vec![sample_info.clone()] },
            DaemonResponse::WallpaperInfo { item: sample_info },
            DaemonResponse::VolumeState { volume: 0.75, muted: false, current_id: None },
            DaemonResponse::VolumeState { volume: 0.5,  muted: true,  current_id: Some(99) },
            DaemonResponse::ScanProgress { done: 5, total: 100 },
            DaemonResponse::OutputList { names: vec!["DP-1".into(), "HDMI-A-1".into()], resolutions: vec![(1920, 1080), (2560, 1440)] },
            DaemonResponse::WorkspaceMap { map: std::collections::HashMap::new() },
            DaemonResponse::WorkspaceMap { map: [("1".to_owned(), 42u64), ("2".to_owned(), 99u64)].into() },
        ];

        for resp in &responses {
            let json = serde_json::to_string(resp)?;
            let back: DaemonResponse = serde_json::from_str(&json)?;
            assert_eq!(resp, &back, "round-trip failed for {:?}", resp);
        }

        // Spot-check wire formats
        assert_eq!(serde_json::to_string(&DaemonResponse::Ok)?, r#"{"type":"Ok"}"#);
        assert_eq!(
            serde_json::to_string(&DaemonResponse::ScanProgress { done: 3, total: 10 })?,
            r#"{"type":"ScanProgress","done":3,"total":10}"#,
        );
        // VolumeState.current_id=None must be omitted (backward compat with v0.1 daemons).
        let vs = DaemonResponse::VolumeState { volume: 1.0, muted: false, current_id: None };
        assert!(!serde_json::to_string(&vs)?.contains("current_id"),
            "current_id=None must be omitted from wire format");

        Ok(())
    }

    // ── Async pipe round-trips ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_send_recv_command_over_pipe() -> crate::error::Result<()> {
        // Exercise several command types including ones with payload fields.
        let commands = vec![
            ClientCommand::List,
            ClientCommand::Set    { id: 42, monitor: None },
            ClientCommand::Volume { level: 0.7 },
            ClientCommand::Info   { id: 99 },
            ClientCommand::Kill,
        ];
        for cmd in &commands {
            let (client, server) = tokio::io::duplex(1024);
            let (server_read, _) = tokio::io::split(server);
            let (_, mut client_write) = tokio::io::split(client);
            let mut server_reader = tokio::io::BufReader::new(server_read);
            send_command(&mut client_write, cmd).await?;
            let received = recv_command(&mut server_reader).await?;
            assert_eq!(&received, cmd, "pipe round-trip failed for {:?}", cmd);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_send_recv_response_over_pipe() -> crate::error::Result<()> {
        // Cover response types that carry payload, including ScanProgress.
        let responses = vec![
            DaemonResponse::Ok,
            DaemonResponse::ScanProgress { done: 3, total: 10 },
            DaemonResponse::VolumeState { volume: 0.5, muted: true, current_id: Some(7) },
        ];
        for resp in &responses {
            let (client, server) = tokio::io::duplex(4096);
            let (_, mut client_write) = tokio::io::split(client);
            let (server_read, _)  = tokio::io::split(server);
            let mut server_reader = tokio::io::BufReader::new(server_read);
            send_response(&mut client_write, resp).await?;
            let received = recv_response(&mut server_reader).await?;
            assert_eq!(&received, resp, "pipe round-trip failed for {:?}", resp);
        }
        Ok(())
    }

    // ── EOF handling ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_eof_returns_ipc_closed_for_command() {
        let empty: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(empty);
        let result = recv_command(&mut reader).await;
        assert!(
            matches!(result, Err(WpickError::IpcClosed)),
            "expected IpcClosed, got {:?}", result
        );
    }

    #[tokio::test]
    async fn test_eof_returns_ipc_closed_for_response() {
        let empty: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(empty);
        let result = recv_response(&mut reader).await;
        assert!(
            matches!(result, Err(WpickError::IpcClosed)),
            "expected IpcClosed, got {:?}", result
        );
    }

    // ── Oversized message protection ──────────────────────────────────────────

    #[tokio::test]
    async fn test_oversized_command_rejected() {
        // A line just over MAX_CMD_BYTES must return IpcProtocol, not deserialize.
        let payload = "x".repeat(MAX_CMD_BYTES + 1);
        let line = format!("{payload}\n");
        let mut reader = tokio::io::BufReader::new(line.as_bytes());
        let result = recv_command(&mut reader).await;
        assert!(
            matches!(result, Err(WpickError::IpcProtocol(_))),
            "expected IpcProtocol for oversized command, got {:?}", result
        );
    }
}
