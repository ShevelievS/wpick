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
        let resp = ipc::recv_response(&mut self.reader)
            .await
            .context("recv response")?;
        Ok(resp)
    }

    pub async fn list_wallpapers(&mut self) -> Result<Vec<WallpaperInfo>> {
        match self.send(&ClientCommand::List).await? {
            DaemonResponse::WallpaperList { items } => Ok(items),
            DaemonResponse::Error { message } => anyhow::bail!("{}", message),
            other => anyhow::bail!("unexpected response: {:?}", other),
        }
    }

    pub async fn set_wallpaper(&mut self, id: u64) -> Result<()> {
        match self.send(&ClientCommand::Set { id }).await? {
            DaemonResponse::Ok => Ok(()),
            DaemonResponse::Error { message } => anyhow::bail!("{}", message),
            other => anyhow::bail!("unexpected response: {:?}", other),
        }
    }
}
