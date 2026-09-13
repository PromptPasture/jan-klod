//! Minisign keypairs and signatures, built here because nothing on the machine
//! can make one.
//!
//! # Why this exists rather than a signing dependency
//!
//! `ext install` must be tested for *accepting* a valid signature, not only for
//! refusing bad ones — a suite of refusals alone passes with a verifier that
//! rejects everything, which is this repository's recurring defect. That needs
//! a signature, and there is no `minisign` binary here. The `minisign` crate
//! signs, but costs 10 packages and brings `rpassword` and `scrypt` in for
//! interactive password prompts (measured; `schemars` was rejected at 7).
//!
//! So the format is built directly from `ring` (Ed25519, already in the lock
//! via rustls) and `blake2` (one package, pinned to 0.10 so it shares
//! `digest 0.10` with `sha2`). ~60 lines instead of 10 crates.
//!
//! # Why building it by hand is sound here
//!
//! The worry with a hand-made fixture is that it is subtly off-spec, gets
//! rejected, and the rejection is mistaken for the verifier working. That does
//! not apply: **the fixture only has to satisfy the same verifier that will
//! check real signatures.** `minisign-verify` is third-party code this repo
//! does not write, so a signature it accepts from here is one it would accept
//! from minisign itself — the two go through identical parsing and
//! verification. The one way to get this wrong is to exercise a *different*
//! path than real signatures take, which is why [`sign`] produces the
//! **prehashed** form (`ED`) that modern minisign emits and the installer
//! requires, and why [`sign_legacy`] exists to prove the legacy path is
//! refused rather than silently accepted.
//!
//! # The format, read out of `minisign-verify 0.2.5`'s own source
//!
//! A `.minisig` is four lines:
//!
//! 1. `untrusted comment: <anything>`
//! 2. base64 of 74 bytes — `sigalg[2] || key_id[8] || signature[64]`
//! 3. `trusted comment: <text>` — the prefix is required
//! 4. base64 of `global_signature[64]`
//!
//! `sigalg` is `ED` for prehashed (Ed25519 over `BLAKE2b-512` of the file) and
//! `Ed` for legacy (over the raw file). The global signature covers
//! `signature || trusted_comment_text` — the text **after** the 17-character
//! prefix, not the whole line, which is the detail easiest to get wrong.

use std::path::Path;

use blake2::digest::consts::U64;
use blake2::{Blake2b, Digest};
use ring::signature::{Ed25519KeyPair, KeyPair};

/// Minisign's prehashed algorithm tag.
const PREHASHED: [u8; 2] = [0x45, 0x44];
/// Minisign's legacy algorithm tag: Ed25519 over the raw file.
const LEGACY: [u8; 2] = [0x45, 0x64];
/// The prefix line 3 must carry, and whose length line 4's input skips.
const TRUSTED_PREFIX: &str = "trusted comment: ";

/// A throwaway signing identity.
pub struct Signer {
    pair: Ed25519KeyPair,
    key_id: [u8; 8],
}

impl Signer {
    /// A fresh keypair, with a key id derived from the public key.
    ///
    /// The id is arbitrary in minisign — it only has to match between the
    /// public key and the signatures made with it — so the first eight bytes of
    /// the public key serve, and two different `Signer`s therefore differ.
    pub fn new() -> Self {
        let seed = ring::rand::SystemRandom::new();
        let document =
            Ed25519KeyPair::generate_pkcs8(&seed).expect("the platform can generate a keypair");
        let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).expect("its own key parses");
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&pair.public_key().as_ref()[..8]);
        Self { pair, key_id }
    }

    /// This identity's public key, base64 as minisign writes it:
    /// `sigalg[2] || key_id[8] || key[32]`.
    pub fn public_key_base64(&self) -> String {
        let mut raw = Vec::with_capacity(42);
        raw.extend_from_slice(&PREHASHED);
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(self.pair.public_key().as_ref());
        base64(&raw)
    }

    /// Write `<file>.minisig` for `file`, in the prehashed form the installer
    /// requires.
    pub fn sign(&self, file: &Path) {
        self.write_signature(file, PREHASHED);
    }

    /// The same, in the **legacy** form — for proving it is refused.
    pub fn sign_legacy(&self, file: &Path) {
        self.write_signature(file, LEGACY);
    }

    fn write_signature(&self, file: &Path, sigalg: [u8; 2]) {
        let content = std::fs::read(file).expect("reads the file being signed");
        let signed: Vec<u8> = if sigalg == PREHASHED {
            Blake2b::<U64>::digest(&content).to_vec()
        } else {
            content
        };
        let signature = self.pair.sign(&signed);

        let trusted = "signed by the test suite";
        let mut line2 = Vec::with_capacity(74);
        line2.extend_from_slice(&sigalg);
        line2.extend_from_slice(&self.key_id);
        line2.extend_from_slice(signature.as_ref());

        // The global signature covers the signature bytes plus the trusted
        // comment *text*, which is what makes that comment tamper-evident.
        let mut global_input = Vec::new();
        global_input.extend_from_slice(signature.as_ref());
        global_input.extend_from_slice(trusted.as_bytes());
        let global = self.pair.sign(&global_input);

        let text = format!(
            "untrusted comment: signature from the jan-klod test suite\n{}\n{TRUSTED_PREFIX}{trusted}\n{}\n",
            base64(&line2),
            base64(global.as_ref())
        );
        let mut path = file.as_os_str().to_os_string();
        path.push(".minisig");
        std::fs::write(Path::new(&path), text).expect("writes the signature");
    }
}

/// Standard base64, which is what minisign uses for every binary field.
///
/// Hand-written for the same reason as the rest of this module: the crate that
/// would provide it is not in the tree, and 20 lines is cheaper than a
/// dependency for test fixtures.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
