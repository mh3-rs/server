//! A console's side of the Nintendo login (`/ac`, `../docs/history/nintendo login.md`): a form posted over plain HTTP, as the game
//! does.

use std::net::SocketAddr;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Posts a form to `/ac`: the reply's fields, or an error on any status but 200.
pub async fn ac(addr: SocketAddr, fields: &[(&str, &[u8])]) -> Result<Vec<(String, Vec<u8>)>> {
    // URL-encoded on top, as the game posts it: its alphabet's only character that needs it is the padding.
    post(
        addr,
        &wire::nas::encode(fields).trim_end().replace('*', "%2A"),
    )
    .await
}

/// Posts a body to `/ac` as it is (a recorded one).
pub async fn post(addr: SocketAddr, body: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut s = tokio::net::TcpStream::connect(addr).await?;
    let request = format!(
        "POST /ac HTTP/1.0\r\nUser-Agent: RVL SDK/1.0\r\nHost: naswii.nintendowifi.net\r\n\
         Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(request.as_bytes()).await?;
    let mut out = Vec::new();
    s.read_to_end(&mut out).await?;
    let text = String::from_utf8_lossy(&out);
    let (head, body) = text.split_once("\r\n\r\n").context("no HTTP head")?;
    let status = head.split_whitespace().nth(1).unwrap_or_default();
    if status != "200" {
        bail!("NAS answered {status}");
    }
    wire::nas::decode(body.as_bytes()).context("a malformed form")
}

/// The `svcloc` step: the `servicetoken` a console then carries to OPN and LMP.
pub async fn svcloc(addr: SocketAddr, macadr: &str, csnum: &str) -> Result<Vec<u8>> {
    let reply = ac(
        addr,
        &[
            ("action", b"svcloc"),
            ("svc", b""),
            ("userid", b"0000000000002"),
            ("macadr", macadr.as_bytes()),
            ("csnum", csnum.as_bytes()),
        ],
    )
    .await?;
    reply
        .into_iter()
        .find(|(k, _)| k == "servicetoken")
        .map(|(_, v)| v)
        .context("no servicetoken")
}
