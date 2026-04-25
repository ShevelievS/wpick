use serde::{Deserialize, Serialize};

use crate::error::{Result, WpickError};
use crate::model::WallpaperInfo;

// ─── Protocol enums ───────────────────────────────────────────────────────────

/// Commands sent from wpick-tui to wpick-daemon over the Unix socket.
/// Wire format: `{"type":"List"}`, `{"type":"Set","id":42}`, etc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientCommand {
    List,
    Scan,
    Set    { id: u64 },
    Volume { level: f32 },
    Mute,
    /// Query current volume and mute state without changing anything.
    Status,
    Info   { id: u64 },
    Kill,
}

/// Responses sent from wpick-daemon back to wpick-tui.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonResponse {
    Ok,
    Error         { message: String },
    WallpaperList { items: Vec<WallpaperInfo> },
    WallpaperInfo { item: WallpaperInfo },
    /// Returned by Volume, Mute, and Status — carries the authoritative runtime state.
    /// `current_id` is the active wallpaper Workshop ID, or None when nothing is playing.
    /// `#[serde(default)]` keeps it compatible with v0.1 daemons that don't send the field.
    VolumeState {
        volume:     f32,
        muted:      bool,
        #[serde(default)]
        current_id: Option<u64>,
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
pub async fn recv_command<R>(reader: &mut R) -> Result<ClientCommand>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(WpickError::IpcClosed);
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
pub async fn recv_response<R>(reader: &mut R) -> Result<DaemonResponse>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(WpickError::IpcClosed);
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
            ClientCommand::Set    { id: 42 },
            ClientCommand::Volume { level: 0.5 },
            ClientCommand::Mute,
            ClientCommand::Status,
            ClientCommand::Info   { id: 99 },
            ClientCommand::Kill,
        ];

        for cmd in &commands {
            let json = serde_json::to_string(cmd)?;
            let back: ClientCommand = serde_json::from_str(&json)?;
            assert_eq!(cmd, &back, "round-trip failed for {:?}", cmd);
        }

        // Spot-check wire format
        assert_eq!(serde_json::to_string(&ClientCommand::List)?, r#"{"type":"List"}"#);
        assert_eq!(serde_json::to_string(&ClientCommand::Scan)?, r#"{"type":"Scan"}"#);
        assert_eq!(serde_json::to_string(&ClientCommand::Set { id: 42 })?, r#"{"type":"Set","id":42}"#);
        assert_eq!(serde_json::to_string(&ClientCommand::Volume { level: 0.5 })?, r#"{"type":"Volume","level":0.5}"#);

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
        };

        let responses: Vec<DaemonResponse> = vec![
            DaemonResponse::Ok,
            DaemonResponse::Error { message: "oops".into() },
            DaemonResponse::WallpaperList { items: vec![sample_info.clone()] },
            DaemonResponse::WallpaperInfo { item: sample_info },
            DaemonResponse::VolumeState { volume: 0.75, muted: false, current_id: None },
        ];

        for resp in &responses {
            let json = serde_json::to_string(resp)?;
            let _back: DaemonResponse = serde_json::from_str(&json)?;
        }

        // Spot-check wire format
        assert_eq!(serde_json::to_string(&DaemonResponse::Ok)?, r#"{"type":"Ok"}"#);

        Ok(())
    }

    // ── Async pipe round-trip ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_send_recv_command_over_pipe() -> crate::error::Result<()> {
        let (client, server) = tokio::io::duplex(1024);
        let (server_read, _server_write) = tokio::io::split(server);
        let (_client_read, mut client_write) = tokio::io::split(client);

        let mut server_reader = tokio::io::BufReader::new(server_read);

        send_command(&mut client_write, &ClientCommand::List).await?;

        let received = recv_command(&mut server_reader).await?;
        assert_eq!(received, ClientCommand::List);
        Ok(())
    }

    #[tokio::test]
    async fn test_send_recv_response_over_pipe() -> crate::error::Result<()> {
        let (client, server) = tokio::io::duplex(1024);
        let (_client_read, mut client_write) = tokio::io::split(client);
        let (server_read, _server_write) = tokio::io::split(server);
        let mut server_reader = tokio::io::BufReader::new(server_read);

        send_response(&mut client_write, &DaemonResponse::Ok).await?;

        let received = recv_response(&mut server_reader).await?;
        assert!(matches!(received, DaemonResponse::Ok));
        Ok(())
    }

    #[tokio::test]
    async fn test_eof_returns_ipc_closed_for_command() {
        // Empty reader → EOF → IpcClosed
        let empty: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(empty);
        let result = recv_command(&mut reader).await;
        assert!(
            matches!(result, Err(WpickError::IpcClosed)),
            "expected IpcClosed, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_eof_returns_ipc_closed_for_response() {
        let empty: &[u8] = b"";
        let mut reader = tokio::io::BufReader::new(empty);
        let result = recv_response(&mut reader).await;
        assert!(
            matches!(result, Err(WpickError::IpcClosed)),
            "expected IpcClosed, got {:?}",
            result
        );
    }
}
