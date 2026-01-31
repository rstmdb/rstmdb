//! Stream abstraction for TLS and plain TCP.

use pin_project_lite::pin_project;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream as ServerTlsStream;

pin_project! {
    /// A stream that can be either plain TCP or TLS.
    #[project = MaybeStreamProj]
    pub enum MaybeTlsStream {
        Plain { #[pin] stream: TcpStream },
        Tls { #[pin] stream: ServerTlsStream<TcpStream> },
    }
}

impl MaybeTlsStream {
    /// Returns whether this stream is TLS-encrypted.
    pub fn is_tls(&self) -> bool {
        matches!(self, MaybeTlsStream::Tls { .. })
    }
}

impl AsyncRead for MaybeTlsStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            MaybeStreamProj::Plain { stream } => stream.poll_read(cx, buf),
            MaybeStreamProj::Tls { stream } => stream.poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTlsStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            MaybeStreamProj::Plain { stream } => stream.poll_write(cx, buf),
            MaybeStreamProj::Tls { stream } => stream.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            MaybeStreamProj::Plain { stream } => stream.poll_flush(cx),
            MaybeStreamProj::Tls { stream } => stream.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            MaybeStreamProj::Plain { stream } => stream.poll_shutdown(cx),
            MaybeStreamProj::Tls { stream } => stream.poll_shutdown(cx),
        }
    }
}
