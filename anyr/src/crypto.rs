/*
 * BIP39 mnemonic → Anytype account key/id derivation
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::Result;
use hmac::{Hmac, Mac};
use sha2::Sha512;

type HmacSha512 = Hmac<Sha512>;

/// Derive SLIP-10 master key and chain code from a BIP39 seed.
fn slip10_derive_master(seed: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut mac = HmacSha512::new_from_slice(b"ed25519 seed").expect("valid HMAC key length");
    mac.update(seed);
    let result = mac.finalize().into_bytes();
    let mut key = [0u8; 32];
    let mut chain_code = [0u8; 32];
    key.copy_from_slice(&result[..32]);
    chain_code.copy_from_slice(&result[32..]);
    (key, chain_code)
}

/// Derive a SLIP-10 hardened child key. The `index` is the child index
/// *without* the hardened flag; `0x80000000` is OR-ed in automatically.
fn slip10_derive_child(key: &[u8; 32], chain_code: &[u8; 32], index: u32) -> ([u8; 32], [u8; 32]) {
    let mut mac = HmacSha512::new_from_slice(chain_code).expect("valid HMAC key length");
    mac.update(&[0x00]);
    mac.update(key);
    mac.update(&(0x8000_0000 | index).to_be_bytes());
    let result = mac.finalize().into_bytes();
    let mut child_key = [0u8; 32];
    let mut child_chain = [0u8; 32];
    child_key.copy_from_slice(&result[..32]);
    child_chain.copy_from_slice(&result[32..]);
    (child_key, child_chain)
}

/// Derive a SLIP-10 key at the given path from a BIP39 seed.
/// Each element of `path` is a hardened child index (without the `0x80000000` flag).
fn slip10_derive_path(seed: &[u8], path: &[u32]) -> ([u8; 32], [u8; 32]) {
    let (mut key, mut chain_code) = slip10_derive_master(seed);
    for &index in path {
        let (k, c) = slip10_derive_child(&key, &chain_code, index);
        key = k;
        chain_code = c;
    }
    (key, chain_code)
}

/// Compute CRC-16/XMODEM (polynomial 0x1021, initial value 0).
fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Encode an Ed25519 public key as an Anytype account ID.
///
/// Format: `Base58(0x5b || pubkey[32] || crc16_xmodem_le[2])`
fn encode_account_id(pubkey: &[u8; 32]) -> String {
    let mut payload = Vec::with_capacity(35);
    payload.push(0x5b);
    payload.extend_from_slice(pubkey);
    let crc = crc16_xmodem(&payload);
    payload.extend_from_slice(&crc.to_le_bytes());
    bs58::encode(payload).into_string()
}

/// Derive the Anytype account key (base64) and account ID from a BIP39 mnemonic.
///
/// The derivation follows the any-sync Go implementation:
///   - BIP39 seed from mnemonic (empty passphrase)
///   - SLIP-10 path `m/44'/2046'/0'` → account key (base64 of `key || chain_code`)
///   - SLIP-10 path `m/44'/2046'/0'/0'` → Ed25519 identity key → account ID
pub fn derive_keys_from_mnemonic(mnemonic: &str) -> Result<(String, String)> {
    use base64::Engine;
    use ed25519_dalek::SigningKey;

    let parsed = bip39::Mnemonic::parse_normalized(mnemonic)
        .map_err(|e| anyhow::anyhow!("invalid mnemonic: {e}"))?;

    let seed = parsed.to_seed_normalized("");

    // m/44'/2046'/0' → account key
    let path = [44, 2046, 0];
    let (key, chain_code) = slip10_derive_path(&seed, &path);

    let mut account_key_bytes = [0u8; 64];
    account_key_bytes[..32].copy_from_slice(&key);
    account_key_bytes[32..].copy_from_slice(&chain_code);
    let account_key = base64::engine::general_purpose::STANDARD.encode(account_key_bytes);

    // m/44'/2046'/0'/0' → identity key → account ID
    let (identity_key, _) = slip10_derive_child(&key, &chain_code, 0);
    let signing_key = SigningKey::from_bytes(&identity_key);
    let pubkey_bytes: [u8; 32] = signing_key.verifying_key().to_bytes();
    let account_id = encode_account_id(&pubkey_bytes);

    Ok((account_key, account_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SLIP-10 test vector 1 from the specification.
    /// Seed: 000102030405060708090a0b0c0d0e0f
    #[test]
    fn slip10_test_vector_1() {
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();

        // Chain m
        let (key, cc) = slip10_derive_master(&seed);
        assert_eq!(
            hex::encode(key),
            "2b4be7f19ee27bbf30c667b642d5f4aa69fd169872f8fc3059c08ebae2eb19e7"
        );
        assert_eq!(
            hex::encode(cc),
            "90046a93de5380a72b5e45010748567d5ea02bbf6522f979e05c0d8d8ca9fffb"
        );

        // Chain m/0'
        let (key, cc) = slip10_derive_child(&key, &cc, 0);
        assert_eq!(
            hex::encode(key),
            "68e0fe46dfb67e368c75379acec591dad19df3cde26e63b93a8e704f1dade7a3"
        );
        assert_eq!(
            hex::encode(cc),
            "8b59aa11380b624e81507a27fedda59fea6d0b779a778918a2fd3590e16e9c69"
        );

        // Chain m/0'/1'
        let (key, cc) = slip10_derive_child(&key, &cc, 1);
        assert_eq!(
            hex::encode(key),
            "b1d0bad404bf35da785a64ca1ac54b2617211d2777696fbffaf208f746ae84f2"
        );
        assert_eq!(
            hex::encode(cc),
            "a320425f77d1b5c2505a6b1b27382b37368ee640e3557c315416801243552f14"
        );

        // Chain m/0'/1'/2'
        let (key, cc) = slip10_derive_child(&key, &cc, 2);
        assert_eq!(
            hex::encode(key),
            "92a5b23c0b8a99e37d07df3fb9966917f5d06e02ddbd909c7e184371463e9fc9"
        );
        assert_eq!(
            hex::encode(cc),
            "2e69929e00b5ab250f49c3fb1c12f252de4fed2c1db88387094a0f8c4c9ccd6c"
        );
    }

    /// SLIP-10 path derivation convenience function.
    #[test]
    fn slip10_derive_path_matches_stepwise() {
        let seed = hex::decode("000102030405060708090a0b0c0d0e0f").unwrap();

        let (key, cc) = slip10_derive_path(&seed, &[0, 1, 2]);
        assert_eq!(
            hex::encode(key),
            "92a5b23c0b8a99e37d07df3fb9966917f5d06e02ddbd909c7e184371463e9fc9"
        );
        assert_eq!(
            hex::encode(cc),
            "2e69929e00b5ab250f49c3fb1c12f252de4fed2c1db88387094a0f8c4c9ccd6c"
        );
    }

    /// CRC-16/XMODEM standard check value: "123456789" → 0x31C3
    #[test]
    fn crc16_xmodem_check_value() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }

    /// Round-trip test: decode the test vector, re-encode, and compare.
    /// Test vectors generated with go-slip10.
    #[test]
    fn strkey_round_trip() {
        let encoded = "ABCw4rFBR7qU2HGzHwnKLYo9mMRcjGhFK28gSy58RKc5feqz";
        let decoded = bs58::decode(encoded).into_vec().unwrap();
        assert_eq!(decoded.len(), 35, "expected 1 + 32 + 2 bytes");
        assert_eq!(decoded[0], 0x5b, "version byte");

        let pubkey: [u8; 32] = decoded[1..33].try_into().unwrap();
        let stored_crc = u16::from_le_bytes([decoded[33], decoded[34]]);
        let computed_crc = crc16_xmodem(&decoded[..33]);
        assert_eq!(stored_crc, computed_crc, "CRC mismatch");

        let re_encoded = encode_account_id(&pubkey);
        assert_eq!(re_encoded, encoded);
    }

    /// End-to-end test using test vectors generated with go-slip10.
    ///
    /// Mnemonic: "tag volcano eight thank tide danger coast health above argue embrace heavy"
    /// Expected values generated by running the Go any-sync library.
    #[test]
    fn derive_keys_from_mnemonic_go_vector() {
        let mnemonic = "tag volcano eight thank tide danger coast health above argue embrace heavy";
        let (account_key, account_id) = derive_keys_from_mnemonic(mnemonic).unwrap();
        assert_eq!(
            account_key,
            "2x9TiDKFCAl79l5llFLvI4yU3P8KImRCm/STVr/iIU+leXyZof6C8KRr0666JX7wFvWprtOqnmK+W/1TTYWiTg=="
        );
        assert_eq!(
            account_id,
            "A9ZJ9CkjFnMLw8Lsgt8gnVTBqhrx1fRPbdCSucdpXxVi78WW"
        );
    }
}
