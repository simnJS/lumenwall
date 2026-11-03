//! Minisign signatures and the `SHA256SUMS` list.
//!
//! Every published file is signed in minisign's pre-hashed mode (BLAKE2b-512, then
//! Ed25519). The signature's trusted comment is signed too and must name the file
//! (`file:<name>`): a valid signature of another file, such as an older archive served
//! under a newer name, is refused.

use minisign_verify::{PublicKey, Signature};

use crate::Error;

/// Parses a `.minisig` file.
pub(crate) fn signature(text: &[u8]) -> Result<Signature, Error> {
    let text =
        std::str::from_utf8(text).map_err(|_| Error::Signature("unreadable signature".into()))?;
    Signature::decode(text.trim()).map_err(|_| Error::Signature("unreadable signature".into()))
}

/// Verifies `data` against `sig` and checks that the trusted comment names `file`.
pub(crate) fn bytes(
    key: &PublicKey,
    data: &[u8],
    sig: &Signature,
    file: &str,
) -> Result<(), Error> {
    key.verify(data, sig, false).map_err(|e| why(&e, file))?;
    names(sig, file)
}

/// The trusted comment (authenticated with the signature) names `file`.
pub(crate) fn names(sig: &Signature, file: &str) -> Result<(), Error> {
    let want = format!("file:{file}");
    if sig.trusted_comment().split_whitespace().any(|t| t == want) {
        Ok(())
    } else {
        Err(Error::Signature(format!("the signature is not for {file}")))
    }
}

pub(crate) fn why(e: &minisign_verify::Error, file: &str) -> Error {
    use minisign_verify::Error as E;
    Error::Signature(match e {
        E::UnexpectedKeyId => format!("{file} is signed with another key"),
        E::UnexpectedAlgorithm | E::UnsupportedAlgorithm | E::UnsupportedLegacyMode => {
            format!("{file} uses an unsupported signature type")
        }
        _ => format!("{file} does not match its signature"),
    })
}

/// The SHA-256 listed for `file` in a `sha256sum`-style list (`<hex>  <name>` or
/// `<hex> *<name>`), lowercase.
pub(crate) fn listed_sum(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.trim_end().split_once(char::is_whitespace)?;
        let name = name.trim_start();
        let name = name.strip_prefix('*').unwrap_or(name);
        (name == file && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector of the minisign-verify crate (pre-hashed signature of the bytes "test").
    const PK: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIG: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    #[test]
    fn prehashed_signature_and_file_name() {
        let key = PublicKey::from_base64(PK).unwrap();
        let sig = signature(SIG.as_bytes()).unwrap();
        bytes(&key, b"test", &sig, "test").unwrap();
        assert!(matches!(
            bytes(&key, b"test", &sig, "other"),
            Err(Error::Signature(_))
        ));
        assert!(matches!(
            bytes(&key, b"tesT", &sig, "test"),
            Err(Error::Signature(_))
        ));
        assert!(signature(b"garbage").is_err());
    }

    #[test]
    fn sums_lists() {
        let h = "a".repeat(64);
        let text = format!(
            "{h}  lumenwall-0.2.0-x86_64-unknown-linux-gnu.tar.gz\n{}  *lumenwall-0.2.0-x86_64-pc-windows-msvc.zip\n",
            "B".repeat(64)
        );
        assert_eq!(
            listed_sum(&text, "lumenwall-0.2.0-x86_64-unknown-linux-gnu.tar.gz").as_deref(),
            Some(h.as_str())
        );
        assert_eq!(
            listed_sum(&text, "lumenwall-0.2.0-x86_64-pc-windows-msvc.zip"),
            Some("b".repeat(64))
        );
        assert_eq!(listed_sum(&text, "lumenwall-0.2.0"), None);
        assert_eq!(listed_sum("abc  x\n", "x"), None);
        assert_eq!(hex(&[0, 0xab, 0x10]), "00ab10");
    }
}
