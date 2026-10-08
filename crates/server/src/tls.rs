//! OPN's TLS, set up for the Wii: SSLv3/TLS 1.0 era cipher suites, RSA key exchange, and a 1024-bit
//! CA. Modern defaults (and rustls) refuse all of that, hence OpenSSL at security level 0.

use std::path::Path;

use anyhow::{Context, Result};
use openssl::ssl::{SslContext, SslFiletype, SslMethod};

/// What the Wii offers is `AES128-SHA`/`AES256-SHA`; the rest is the list MH3SP serves, kept for
/// clients we haven't met yet. Suites the local OpenSSL no longer has are skipped.
const WII_CIPHERS: &str =
    "AES128-SHA:AES256-SHA:DES-CBC-SHA:DES-CBC3-SHA:RC4-MD5:RC4-SHA:@SECLEVEL=0";

pub fn context(cert: &Path, key: &Path) -> Result<SslContext> {
    let mut builder = SslContext::builder(SslMethod::tls_server())?;
    builder.set_security_level(0);
    builder.set_min_proto_version(None)?; // the lowest this OpenSSL supports
    builder.set_cipher_list(WII_CIPHERS)?;
    builder
        .set_certificate_chain_file(cert)
        .with_context(|| format!("TLS certificate {}", cert.display()))?;
    builder
        .set_private_key_file(key, SslFiletype::PEM)
        .with_context(|| format!("TLS key {}", key.display()))?;
    builder.check_private_key()?;
    Ok(builder.build())
}
