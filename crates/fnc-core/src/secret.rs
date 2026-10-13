//! Password storage for settings (HTTP server): PBKDF2-HMAC-SHA256, stored as
//! `pbkdf2-sha256$<iterations>$<salt hex>$<hash hex>`. A plain value (older settings,
//! `/http_password=...`) is still accepted and replaced by its hash.

use sha2::{Digest, Sha256};

/// Prefix of stored hashes.
pub const PREFIX: &str = "pbkdf2-sha256$";
/// Work factor (≈ 20 ms on a desktop CPU; verified once per client and cached).
pub const ITERATIONS: u32 = 100_000;

fn hmac_sha256(key: &[u8], msg: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for m in msg {
        inner.update(m);
    }
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(ih);
    outer.finalize().into()
}

/// PBKDF2-HMAC-SHA256, one 32-byte block.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut u = hmac_sha256(password, &[salt, &1u32.to_be_bytes()]);
    let mut out = u;
    for _ in 1..iterations.max(1) {
        u = hmac_sha256(password, &[&u]);
        for (o, x) in out.iter_mut().zip(u) {
            *o ^= x;
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// `true` if `stored` is a hash produced by [`hash`].
pub fn is_hashed(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

/// Hashes `password` with a random salt.
pub fn hash(password: &str) -> String {
    let mut salt = [0u8; 16];
    if getrandom::fill(&mut salt).is_err() {
        // No OS randomness (should not happen): a time-derived salt still differs per call.
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        salt.copy_from_slice(&Sha256::digest(t.to_le_bytes())[..16]);
    }
    let h = pbkdf2_sha256(password.as_bytes(), &salt, ITERATIONS);
    format!("{PREFIX}{ITERATIONS}${}${}", hex(&salt), hex(&h))
}

/// Constant-time equality.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// `true` if `password` matches `stored` (a hash, or a legacy plain value).
pub fn verify(password: &str, stored: &str) -> bool {
    let Some(rest) = stored.strip_prefix(PREFIX) else {
        return !stored.is_empty() && ct_eq(password.as_bytes(), stored.as_bytes());
    };
    let mut it = rest.split('$');
    let (Some(iter), Some(salt), Some(want)) = (it.next(), it.next(), it.next()) else { return false };
    let (Ok(iter), Some(salt), Some(want)) = (iter.parse::<u32>(), unhex(salt), unhex(want)) else { return false };
    if iter == 0 || iter > 10_000_000 {
        return false;
    }
    ct_eq(&pbkdf2_sha256(password.as_bytes(), &salt, iter), &want)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_known_vectors() {
        // RFC 7914 §11 (PBKDF2-HMAC-SHA256, first 32 bytes).
        let out = pbkdf2_sha256(b"passwd", b"salt", 1);
        assert_eq!(hex(&out), "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc");
        let out = pbkdf2_sha256(b"Password", b"NaCl", 80_000);
        assert_eq!(hex(&out), "4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56");
    }

    #[test]
    fn hash_and_verify() {
        let h = hash("s3cret");
        assert!(is_hashed(&h) && h != hash("s3cret"), "salted");
        assert!(verify("s3cret", &h));
        assert!(!verify("S3cret", &h) && !verify("", &h));
        assert!(verify("plain", "plain"), "legacy plain values still work");
        assert!(!verify("", ""), "an empty password never matches");
        assert!(!verify("x", "pbkdf2-sha256$abc$zz$zz"));
    }
}
