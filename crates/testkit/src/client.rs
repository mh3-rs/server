//! A scripted console: connects to a server role the way the Wii does and exchanges PAT frames.

use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_openssl::SslStream;
use tokio_util::codec::Framed;
use wire::{Frame, FrameCodec};

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub struct Console {
    framed: Framed<Box<dyn Io>, FrameCodec>,
}

#[derive(Debug)]
pub enum Received {
    Frame(Frame),
    Closed,
    Timeout,
}

/// TLS as the Wii speaks it: at most TLS 1.0, its cipher suites, and no certificate checks
/// (the patched game trusts our CA; here we only care about what goes through).
fn wii_tls() -> Result<SslConnector> {
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;
    builder.set_verify(SslVerifyMode::NONE);
    builder.set_max_proto_version(Some(SslVersion::TLS1))?;
    builder.set_cipher_list("AES128-SHA:AES256-SHA:@SECLEVEL=0")?;
    Ok(builder.build())
}

impl Console {
    pub async fn connect(addr: SocketAddr, tls: bool) -> Result<Self> {
        let tcp = TcpStream::connect(addr)
            .await
            .with_context(|| format!("connect {addr}"))?;
        tcp.set_nodelay(true)?;
        let io: Box<dyn Io> = if tls {
            let ssl = wii_tls()?.configure()?.into_ssl("mh.capcom.co.jp")?;
            let mut stream = SslStream::new(ssl, tcp)?;
            Pin::new(&mut stream)
                .connect()
                .await
                .with_context(|| format!("TLS handshake with {addr}"))?;
            Box::new(stream)
        } else {
            Box::new(tcp)
        };
        Ok(Self {
            framed: Framed::new(io, FrameCodec),
        })
    }

    pub async fn send(&mut self, frame: Frame) -> Result<()> {
        Ok(self.framed.send(frame).await?)
    }

    pub async fn recv(&mut self, timeout: Duration) -> Result<Received> {
        Ok(
            match tokio::time::timeout(timeout, self.framed.next()).await {
                Ok(Some(frame)) => Received::Frame(frame?),
                Ok(None) => Received::Closed,
                Err(_) => Received::Timeout,
            },
        )
    }
}
