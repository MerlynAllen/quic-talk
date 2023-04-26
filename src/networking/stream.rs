
use quinn::{RecvStream, SendStream};
use anyhow::Result;
/// A Session `open` method returns a Stream instance.
pub(crate) struct Stream {
    pub(crate) recv: RecvStream,
    pub(crate) send: SendStream,
    // pub(crate) handle: tokio::task::JoinHandle<Result<()>>,
}

impl Stream {
    pub(crate) async fn write(&mut self, data: &[u8]) -> Result<usize, anyhow::Error> {
        self.send.write(data).await.map_err(|e| anyhow::anyhow!(e))
    }

}

