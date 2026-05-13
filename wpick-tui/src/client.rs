use anyhow::{Context, Result};
use tokio::io::{BufReader, BufWriter};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use wpick_core::ipc::{self, ClientCommand, DaemonResponse};
use wpick_core::model::WallpaperInfo;

pub struct IpcClient {
    reader: BufReader<OwnedReadHalf>,
    writer: BufWriter<OwnedWriteHalf>,
}

impl IpcClient {
    pub async fn connect(socket_path: &std::path::Path) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(socket_path)
            .await
            .with_context(|| format!(
                "Cannot connect to daemon at {:?}. Start with: wpick-daemon",
                socket_path
            ))?;
        let (r, w) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(r),
            writer: BufWriter::new(w),
        })
    }

    /// Non-failing version for the reconnect loop.
    pub async fn try_connect(socket_path: &std::path::Path) -> Option<Self> {
        Self::connect(socket_path).await.ok()
    }

    pub async fn send(&mut self, cmd: &ClientCommand) -> Result<DaemonResponse> {
        ipc::send_command(&mut self.writer, cmd)
            .await
            .context("send command")?;
        ipc::recv_response(&mut self.reader)
            .await
            .context("recv response")
    }

    /// Send a command without reading a response.  Pair with `recv_resp`.
    pub async fn send_cmd_only(&mut self, cmd: &ClientCommand) -> Result<()> {
        ipc::send_command(&mut self.writer, cmd)
            .await
            .context("send command")
    }

    /// Receive one response.  Pair with `send_cmd_only`.
    pub async fn recv_resp(&mut self) -> Result<DaemonResponse> {
        ipc::recv_response(&mut self.reader)
            .await
            .context("recv response")
    }

    /// Send Scan and drain all ScanProgress messages, calling `on_progress` for each.
    /// Returns the final wallpaper list.  Useful in CLI / non-interactive contexts.
    pub async fn scan_all<F>(&mut self, mut on_progress: F) -> Result<Vec<WallpaperInfo>>
    where
        F: FnMut(usize, usize),
    {
        ipc::send_command(&mut self.writer, &ClientCommand::Scan)
            .await
            .context("send scan")?;
        loop {
            match ipc::recv_response(&mut self.reader).await.context("recv scan")? {
                DaemonResponse::ScanProgress { done, total } => on_progress(done, total),
                DaemonResponse::WallpaperList { items }      => return Ok(items),
                DaemonResponse::Error { message }            => anyhow::bail!("{}", message),
                other => anyhow::bail!("unexpected scan response: {:?}", other),
            }
        }
    }

    pub async fn list_wallpapers(&mut self) -> Result<Vec<WallpaperInfo>> {
        match self.send(&ClientCommand::List).await? {
            DaemonResponse::WallpaperList { items } => Ok(items),
            DaemonResponse::Error { message } => anyhow::bail!("{}", message),
            other => anyhow::bail!("unexpected response: {:?}", other),
        }
    }

}
