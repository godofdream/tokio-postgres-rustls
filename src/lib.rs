use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::future::{FutureExt, TryFutureExt};
use ring::digest;
use rustls::{ClientConfig, Session};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio_postgres::tls::{ChannelBinding, MakeTlsConnect, TlsConnect};
use tokio_rustls::{client::TlsStream, TlsConnector};
use webpki::{DNSName, DNSNameRef};

#[derive(Clone)]
pub struct MakeRustlsConnect {
    config: Arc<ClientConfig>,
}

impl MakeRustlsConnect {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl<S> MakeTlsConnect<S> for MakeRustlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type TlsConnect = RustlsConnect;
    type Error = io::Error;

    fn make_tls_connect(&mut self, hostname: &str) -> io::Result<RustlsConnect> {
        DNSNameRef::try_from_ascii_str(hostname)
            .map(|dns_name| RustlsConnect {
                hostname: dns_name.to_owned(),
                connector: Arc::clone(&self.config).into(),
            })
            .map_err(|_| io::ErrorKind::InvalidInput.into())
    }
}

pub struct RustlsConnect {
    hostname: DNSName,
    connector: TlsConnector,
}

impl<S> TlsConnect<S> for RustlsConnect
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Stream = RustlsStream<S>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<RustlsStream<S>>> + Send>>;

    fn connect(self, stream: S) -> Self::Future {
        self.connector
            .connect(self.hostname.as_ref(), stream)
            .map_ok(|s| RustlsStream(Box::pin(s)))
            .boxed()
    }
}

pub struct RustlsStream<S>(Pin<Box<TlsStream<S>>>);

impl<S> tokio_postgres::tls::TlsStream for RustlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn channel_binding(&self) -> ChannelBinding {
        let (_, session) = self.0.get_ref();
        match session.get_peer_certificates() {
            Some(certs) if certs.len() > 0 => {
                let sha256 = digest::digest(&digest::SHA256, certs[0].as_ref());
                ChannelBinding::tls_server_end_point(sha256.as_ref().into())
            }
            _ => ChannelBinding::none(),
        }
    }
}

impl<S> AsyncRead for RustlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<tokio::io::Result<()>> {
        self.0.as_mut().poll_read(cx, buf)
    }

}

impl<S> AsyncWrite for RustlsStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<tokio::io::Result<usize>> {
        self.0.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<tokio::io::Result<()>> {
        self.0.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<tokio::io::Result<()>> {
        self.0.as_mut().poll_shutdown(cx)
    }

}

#[cfg(test)]
mod tests {
    use futures::future::TryFutureExt;

    #[tokio::test]
    async fn it_works() {
        env_logger::builder().is_test(true).try_init().unwrap();

        let config = rustls::ClientConfig::new();
        let tls = super::MakeRustlsConnect::new(config);
        let (client, conn) =
            tokio_postgres::connect("sslmode=require host=localhost port=5432 user=postgres", tls)
                .await
                .expect("connect");
        tokio::spawn(conn.map_err(|e| panic!("{:?}", e)));
        let stmt = client.prepare("SELECT 1").await.expect("prepare");
        let _ = client.query(&stmt, &[]).await.expect("query");
    }
}
