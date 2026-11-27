//! TLS as the Wii speaks it, from the client's side: the scripted console's and `mh3sp check`'s.

use openssl::error::ErrorStack;
use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode, SslVersion};

/// At most TLS 1.0, the Wii's cipher suites, and no certificate checks: the patched game trusts the deployment's own
/// CA, which a client elsewhere (a test, `mh3sp check` run from another machine) doesn't have. What it reaches is
/// proven by what answers, not by the certificate.
pub fn wii_client() -> Result<SslConnector, ErrorStack> {
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;
    builder.set_verify(SslVerifyMode::NONE);
    builder.set_max_proto_version(Some(SslVersion::TLS1))?;
    builder.set_cipher_list("AES128-SHA:AES256-SHA:@SECLEVEL=0")?;
    Ok(builder.build())
}
