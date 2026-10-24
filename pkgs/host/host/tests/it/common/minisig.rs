//! Hand-built minisign keypairs and signatures, verified by the same `minisign-verify` the installer uses.

use std::path::Path;

use blake2::digest::consts::U64;
use blake2::{Blake2b, Digest};
use ring::signature::{Ed25519KeyPair, KeyPair};

const PREHASHED: [u8; 2] = [0x45, 0x44];
const LEGACY: [u8; 2] = [0x45, 0x64];
/// The prefix line 3 must carry, and whose length line 4's input skips.
const TRUSTED_PREFIX: &str = "trusted comment: ";

pub struct Signer {
    pair: Ed25519KeyPair,
    key_id: [u8; 8],
}

impl Signer {
    pub fn new() -> Self {
        let seed = ring::rand::SystemRandom::new();
        let document =
            Ed25519KeyPair::generate_pkcs8(&seed).expect("the platform can generate a keypair");
        let pair = Ed25519KeyPair::from_pkcs8(document.as_ref()).expect("its own key parses");
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&pair.public_key().as_ref()[..8]);
        Self { pair, key_id }
    }

    pub fn public_key_base64(&self) -> String {
        let mut raw = Vec::with_capacity(42);
        raw.extend_from_slice(&PREHASHED);
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(self.pair.public_key().as_ref());
        base64(&raw)
    }

    pub fn sign(&self, file: &Path) {
        self.write_signature(file, PREHASHED);
    }

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
