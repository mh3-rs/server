//! OPN's TLS, set up for the Wii: SSLv3/TLS 1.0 era cipher suites, RSA key exchange, and a 1024-bit
//! CA. Modern defaults (and rustls) refuse all of that, hence OpenSSL at security level 0.
//!
//! Each deployment has its own CA, in `tls_dir` (`../docs/history/patch and CA.md` D1–D3): the server creates it at
//! its first start, and the players' patch puts it in the game in place of Nintendo's. Its certificate is the shape of the one it
//! replaces (RSA-1024, sha1WithRSA, the same extensions, DER exactly 924 bytes: the game's size word stays), under a
//! name of our own. OPN's certificate comes from it, named `mh.capcom.co.jp`: the name the game checks.

use std::path::Path;

use anyhow::{Context, Result, bail};
use openssl::asn1::{Asn1Integer, Asn1Time};
use openssl::bn::BigNum;
use openssl::hash::MessageDigest;
use openssl::pkey::{PKey, Private};
use openssl::rand::rand_bytes;
use openssl::rsa::Rsa;
use openssl::ssl::{SslContext, SslFiletype, SslMethod};
use openssl::x509::extension::{AuthorityKeyIdentifier, BasicConstraints, SubjectKeyIdentifier};
use openssl::x509::{X509, X509Name, X509NameBuilder};

/// What the Wii offers is `AES128-SHA`/`AES256-SHA`; the rest is the list MH3SP serves, kept for
/// clients we haven't met yet. Suites the local OpenSSL no longer has are skipped.
const WII_CIPHERS: &str =
    "AES128-SHA:AES256-SHA:DES-CBC-SHA:DES-CBC3-SHA:RC4-MD5:RC4-SHA:@SECLEVEL=0";

/// The size of the CA the game holds (`pat_ca_cert_size`): ours must match it.
pub const CA_SIZE: usize = 924;
/// The name the game checks OPN's certificate against (`pat_server_host`).
const SERVER_NAME: &str = "mh.capcom.co.jp";
/// UTCTime (before 2050), so a certificate's length doesn't depend on its dates; from 2010, so a console's wrong
/// clock doesn't matter.
const NOT_BEFORE: &str = "100101000000Z";
const NOT_AFTER: &str = "491231235959Z";

const CA_KEY: &str = "ca.key";
const CA_CERT: &str = "ca.der";
const SERVER_KEY: &str = "server.key";
const SERVER_CERT: &str = "server.crt";

pub fn context(dir: &Path) -> Result<SslContext> {
    let (cert, key) = (dir.join(SERVER_CERT), dir.join(SERVER_KEY));
    let mut builder = SslContext::builder(SslMethod::tls_server())?;
    builder.set_security_level(0);
    builder.set_min_proto_version(None)?; // the lowest this OpenSSL supports
    builder.set_cipher_list(WII_CIPHERS)?;
    builder
        .set_certificate_chain_file(&cert)
        .with_context(|| format!("TLS certificate {}", cert.display()))?;
    builder
        .set_private_key_file(&key, SslFiletype::PEM)
        .with_context(|| format!("TLS key {}", key.display()))?;
    builder.check_private_key()?;
    Ok(builder.build())
}

/// Creates what `dir` lacks: everything when it's empty; OPN's key and certificate when only the CA is there (the
/// rotation: players keep their patch). Any other mix is an error: a new CA would make every patch useless.
pub fn prepare(dir: &Path) -> Result<()> {
    let present = |name: &str| dir.join(name).exists();
    let has_ca = (present(CA_KEY), present(CA_CERT));
    let has_server = (present(SERVER_KEY), present(SERVER_CERT));
    match (has_ca, has_server) {
        ((true, true), (true, true)) => return Ok(()),
        ((false, false), (false, false)) => {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
            let (key, cert) = ca()?;
            write(dir, CA_KEY, &key.private_key_to_pem_pkcs8()?, true)?;
            write(dir, CA_CERT, &cert.to_der()?, false)?;
            tracing::info!(dir = %dir.display(), "a new CA: players need a patch made from it");
        }
        ((true, true), (false, false)) => {}
        _ => {
            let missing: Vec<_> = [CA_KEY, CA_CERT, SERVER_KEY, SERVER_CERT]
                .into_iter()
                .filter(|name| !present(name))
                .collect();
            bail!(
                "{}: {} missing; with the CA, delete only {SERVER_KEY} and {SERVER_CERT} to issue new ones, or \
                 empty the folder for a new CA (and a new patch for every player)",
                dir.display(),
                missing.join(", ")
            );
        }
    }
    let ca_key = PKey::private_key_from_pem(&read(dir, CA_KEY)?)?;
    let ca_cert = X509::from_der(&read(dir, CA_CERT)?)?;
    let (key, cert) = server(&ca_key, &ca_cert)?;
    write(dir, SERVER_KEY, &key.private_key_to_pem_pkcs8()?, true)?;
    write(dir, SERVER_CERT, &cert.to_pem()?, false)?;
    tracing::info!(dir = %dir.display(), "a new OPN certificate");
    Ok(())
}

/// The CA's certificate (DER), what the patch puts in the game.
pub fn ca_der(dir: &Path) -> Result<Vec<u8>> {
    read(dir, CA_CERT)
}

fn read(dir: &Path, name: &str) -> Result<Vec<u8>> {
    let path = dir.join(name);
    std::fs::read(&path).with_context(|| format!("read {}", path.display()))
}

/// A key is readable by its owner only.
fn write(dir: &Path, name: &str, bytes: &[u8], secret: bool) -> Result<()> {
    use std::io::Write;
    let path = dir.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if secret {
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    }
    #[cfg(not(unix))]
    let _ = secret;
    options
        .open(&path)
        .and_then(|mut f| f.write_all(bytes))
        .with_context(|| format!("write {}", path.display()))
}

/// A CA of exactly [`CA_SIZE`] bytes. Its name is ours, padded with a random id: the name appears three times
/// (issuer, subject, the authority key identifier) and the serial twice, so some padding length and serial length
/// land on the size. The padding grows until one does (about 100 characters: signing is cheap).
fn ca() -> Result<(PKey<Private>, X509)> {
    let key = PKey::from_rsa(Rsa::generate(1024)?)?;
    let mut id = [0u8; 128];
    rand_bytes(&mut id)?;
    let id = hex::encode(id);
    for pad in 0..=id.len() {
        for serial in 1..=3 {
            let cert = ca_cert(&key, &id[..pad], serial)?;
            match cert.to_der()?.len() {
                CA_SIZE => return Ok((key, cert)),
                len if len > CA_SIZE && serial == 1 => bail!("no CA of {CA_SIZE} bytes"),
                _ => {}
            }
        }
    }
    bail!("no CA of {CA_SIZE} bytes");
}

fn ca_cert(key: &PKey<Private>, pad: &str, serial: usize) -> Result<X509> {
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text("O", "mh3sp")?;
    // At most 64 characters per entry (X.520).
    for chunk in pad.as_bytes().chunks(64) {
        name.append_entry_by_text("OU", std::str::from_utf8(chunk)?)?;
    }
    name.append_entry_by_text("CN", "mh3sp CA")?;
    let name = name.build();
    // 0x01, 0x0101, 0x010101: always positive, its length in bytes is `serial`.
    let serial = BigNum::from_slice(&vec![1; serial])?;
    certificate(key, &name, &serial.to_asn1_integer()?, true, None)
}

fn server(ca_key: &PKey<Private>, ca: &X509) -> Result<(PKey<Private>, X509)> {
    let key = PKey::from_rsa(Rsa::generate(1024)?)?;
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text("CN", SERVER_NAME)?;
    let mut serial = [0u8; 8];
    rand_bytes(&mut serial)?;
    serial[0] &= 0x7f;
    let serial = BigNum::from_slice(&serial)?.to_asn1_integer()?;
    let cert = certificate(&key, &name.build(), &serial, false, Some((ca_key, ca)))?;
    Ok((key, cert))
}

/// The extensions of the certificates the game was built for: subject and authority key identifiers (the
/// authority's name and serial included), basic constraints.
fn certificate(
    key: &PKey<Private>,
    subject: &X509Name,
    serial: &Asn1Integer,
    is_ca: bool,
    issuer: Option<(&PKey<Private>, &X509)>,
) -> Result<X509> {
    let mut b = X509::builder()?;
    b.set_version(2)?;
    b.set_serial_number(serial)?;
    b.set_subject_name(subject)?;
    b.set_issuer_name(issuer.map_or(subject, |(_, ca)| ca.subject_name()))?;
    b.set_pubkey(key)?;
    let (not_before, not_after) = (
        Asn1Time::from_str(NOT_BEFORE)?,
        Asn1Time::from_str(NOT_AFTER)?,
    );
    b.set_not_before(&not_before)?;
    b.set_not_after(&not_after)?;
    let ski =
        SubjectKeyIdentifier::new().build(&b.x509v3_context(issuer.map(|(_, ca)| &**ca), None))?;
    b.append_extension(ski)?;
    let aki = AuthorityKeyIdentifier::new()
        .keyid(true)
        .issuer(true)
        .build(&b.x509v3_context(issuer.map(|(_, ca)| &**ca), None))?;
    b.append_extension(aki)?;
    let mut constraints = BasicConstraints::new();
    if is_ca {
        constraints.ca();
    }
    b.append_extension(constraints.build()?)?;
    b.sign(issuer.map_or(key, |(k, _)| k), MessageDigest::sha1())?;
    Ok(b.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use openssl::nid::Nid;

    #[test]
    fn a_new_folder_gets_a_ca_of_the_games_shape_and_a_certificate_from_it() {
        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path()).unwrap();
        let der = ca_der(dir.path()).unwrap();
        assert_eq!(der.len(), CA_SIZE);
        let ca = X509::from_der(&der).unwrap();
        assert_eq!(
            ca.signature_algorithm().object().nid(),
            Nid::SHA1WITHRSAENCRYPTION
        );
        assert_eq!(ca.public_key().unwrap().bits(), 1024);
        let text = String::from_utf8(ca.to_text().unwrap()).unwrap();
        assert!(text.contains("CA:TRUE"), "{text}");
        assert_eq!(
            ca.subject_name().to_der().unwrap(),
            ca.issuer_name().to_der().unwrap()
        );
        let server = X509::from_pem(&read(dir.path(), SERVER_CERT).unwrap()).unwrap();
        assert!(server.verify(&ca.public_key().unwrap()).unwrap());
        let cn = server
            .subject_name()
            .entries_by_nid(Nid::COMMONNAME)
            .next()
            .unwrap();
        assert_eq!(cn.data().to_string().unwrap(), SERVER_NAME);
        assert_eq!(server.public_key().unwrap().bits(), 1024);
        context(dir.path()).unwrap();
        // Done: a second start changes nothing.
        prepare(dir.path()).unwrap();
        assert_eq!(ca_der(dir.path()).unwrap(), der);
    }

    #[test]
    fn the_ca_size_holds_for_any_key() {
        for _ in 0..20 {
            assert_eq!(ca().unwrap().1.to_der().unwrap().len(), CA_SIZE);
        }
    }

    #[test]
    fn without_its_certificate_the_server_gets_a_new_one_from_the_same_ca() {
        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path()).unwrap();
        let der = ca_der(dir.path()).unwrap();
        let old = read(dir.path(), SERVER_CERT).unwrap();
        for name in [SERVER_KEY, SERVER_CERT] {
            std::fs::remove_file(dir.path().join(name)).unwrap();
        }
        prepare(dir.path()).unwrap();
        assert_eq!(ca_der(dir.path()).unwrap(), der);
        let new = X509::from_pem(&read(dir.path(), SERVER_CERT).unwrap()).unwrap();
        assert_ne!(new.to_pem().unwrap(), old);
        let ca = X509::from_der(&der).unwrap();
        assert!(new.verify(&ca.public_key().unwrap()).unwrap());
    }

    #[test]
    fn any_other_partial_folder_is_an_error_naming_what_is_missing() {
        for keep in [
            &[CA_CERT, SERVER_KEY, SERVER_CERT][..],
            &[CA_KEY, CA_CERT, SERVER_CERT],
            &[SERVER_KEY],
        ] {
            let dir = tempfile::tempdir().unwrap();
            prepare(dir.path()).unwrap();
            for name in [CA_KEY, CA_CERT, SERVER_KEY, SERVER_CERT] {
                if !keep.contains(&name) {
                    std::fs::remove_file(dir.path().join(name)).unwrap();
                }
            }
            let error = prepare(dir.path()).unwrap_err().to_string();
            let (missing, _) = error.split_once(" missing").expect(&error);
            for name in [CA_KEY, CA_CERT, SERVER_KEY, SERVER_CERT] {
                assert_eq!(
                    missing.contains(name),
                    !keep.contains(&name),
                    "{keep:?}: {error}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn keys_are_the_owners_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        prepare(dir.path()).unwrap();
        for name in [CA_KEY, SERVER_KEY] {
            let mode = std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{name}");
        }
    }
}
