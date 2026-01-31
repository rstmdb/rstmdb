//! Client stream abstraction for TLS and plain TCP.

use pin_project_lite::pin_project;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream as ClientTlsStream;

pin_project! {
    /// A client stream that can be either plain TCP or TLS.
    #[project = ClientStreamProj]
    pub enum ClientStream {
        Plain { #[pin] stream: TcpStream },
        Tls { #[pin] stream: ClientTlsStream<TcpStream> },
    }
}

impl ClientStream {
    /// Returns whether this stream is TLS-encrypted.
    pub fn is_tls(&self) -> bool {
        matches!(self, ClientStream::Tls { .. })
    }
}

impl AsyncRead for ClientStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            ClientStreamProj::Plain { stream } => stream.poll_read(cx, buf),
            ClientStreamProj::Tls { stream } => stream.poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ClientStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            ClientStreamProj::Plain { stream } => stream.poll_write(cx, buf),
            ClientStreamProj::Tls { stream } => stream.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            ClientStreamProj::Plain { stream } => stream.poll_flush(cx),
            ClientStreamProj::Tls { stream } => stream.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            ClientStreamProj::Plain { stream } => stream.poll_shutdown(cx),
            ClientStreamProj::Tls { stream } => stream.poll_shutdown(cx),
        }
    }
}
