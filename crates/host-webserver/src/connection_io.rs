//! The socket transport owns cancellation across HTTP requests and protocol upgrades.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use seekdeep_llm::AbortSignal;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
};

pub(super) struct ConnectionIo {
    stream: TcpStream,
    signal: AbortSignal,
}

impl ConnectionIo {
    pub(super) fn new(stream: TcpStream, signal: AbortSignal) -> Self {
        Self { stream, signal }
    }

    fn cancel_on_error<T>(&self, result: &Poll<io::Result<T>>) {
        if matches!(result, Poll::Ready(Err(_))) {
            self.signal.abort();
        }
    }
}

impl Drop for ConnectionIo {
    fn drop(&mut self) {
        self.signal.abort();
    }
}

impl AsyncRead for ConnectionIo {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let remaining = buffer.remaining();
        let filled = buffer.filled().len();
        let result = Pin::new(&mut this.stream).poll_read(context, buffer);
        if remaining > 0
            && buffer.filled().len() == filled
            && matches!(&result, Poll::Ready(Ok(())))
        {
            this.signal.abort();
        }
        this.cancel_on_error(&result);
        result
    }
}

impl AsyncWrite for ConnectionIo {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_write(context, buffer);
        this.cancel_on_error(&result);
        result
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_write_vectored(context, buffers);
        this.cancel_on_error(&result);
        result
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_flush(context);
        this.cancel_on_error(&result);
        result
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.stream).poll_shutdown(context);
        this.cancel_on_error(&result);
        result
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let (client, server) = tokio::join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept()
        );
        (server.unwrap().0, client.unwrap())
    }

    #[tokio::test]
    async fn vectored_writes_reads_and_half_close_retain_the_connection_owner() {
        let (server, mut client) = pair().await;
        let signal = AbortSignal::default();
        let mut io = ConnectionIo::new(server, signal.clone());
        assert!(io.is_write_vectored());
        let mut buffers = [io::IoSlice::new(b"pi"), io::IoSlice::new(b"ng")];
        let mut remaining = buffers.as_mut_slice();
        while !remaining.is_empty() {
            let written = io.write_vectored(remaining).await.unwrap();
            assert_ne!(written, 0);
            io::IoSlice::advance_slices(&mut remaining, written);
        }
        io.flush().await.unwrap();
        let mut bytes = [0; 4];
        client.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"ping");
        client.write_all(b"pong").await.unwrap();
        assert_eq!(io.read(&mut []).await.unwrap(), 0);
        assert!(!signal.is_aborted());
        io.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"pong");
        io.shutdown().await.unwrap();
        assert!(!signal.is_aborted());
        client.shutdown().await.unwrap();
        assert_eq!(io.read(&mut bytes).await.unwrap(), 0);
        assert!(signal.is_aborted());

        let (server, _client) = pair().await;
        let signal = AbortSignal::default();
        let io = ConnectionIo::new(server, signal.clone());
        assert!(!signal.is_aborted());
        drop(io);
        assert!(signal.is_aborted());
    }

    async fn reset_peer(server: &mut TcpStream, client: TcpStream) {
        server.write_all(b"unread").await.unwrap();
        let mut probe = [0];
        assert_eq!(client.peek(&mut probe).await.unwrap(), 1);
        // Closing with unread bytes produces a reset rather than an orderly EOF.
        drop(client);
    }

    #[tokio::test]
    async fn peer_reset_cancels_read_and_write_operations() {
        let (mut server, client) = pair().await;
        reset_peer(&mut server, client).await;
        let read_signal = AbortSignal::default();
        let mut io = ConnectionIo::new(server, read_signal.clone());
        assert!(io.read(&mut [0]).await.is_err());
        assert!(read_signal.is_aborted());

        let (mut server, client) = pair().await;
        reset_peer(&mut server, client).await;
        assert!(server.read(&mut [0]).await.is_err());
        let write_signal = AbortSignal::default();
        let mut io = ConnectionIo::new(server, write_signal.clone());
        assert!(io.write(b"closed").await.is_err());
        assert!(write_signal.is_aborted());
    }
}
