// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

use base64::{Engine as _, engine::general_purpose};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_DECODE_DEPTH: usize = 3;
const MAX_DECODE_BYTES: usize = 4 * 1024 * 1024;

pub struct SecretSet {
    values: Vec<Zeroizing<Vec<u8>>>,
    patterns: Vec<Zeroizing<Vec<u8>>>,
    longest: usize,
}

impl SecretSet {
    pub fn from_values(values: Vec<Zeroizing<Vec<u8>>>) -> Result<Self, String> {
        let mut patterns = Vec::new();
        for value in &values {
            if value.len() < 4 || value.len() > MAX_SECRET_BYTES {
                return Err("credential value has an invalid length".to_owned());
            }
            let raw = Zeroizing::new(value.to_vec());
            add_pattern(&mut patterns, raw.to_vec());
            add_pattern(&mut patterns, hex(&raw, false));
            add_pattern(&mut patterns, hex(&raw, true));
            add_pattern(&mut patterns, percent_encode(&raw, false));
            add_pattern(&mut patterns, percent_encode(&raw, true));
            let percent_lower = Zeroizing::new(percent_encode(&raw, false));
            let percent_upper = Zeroizing::new(percent_encode(&raw, true));
            for percent in [&percent_lower, &percent_upper] {
                add_base64_patterns(&mut patterns, percent);
            }
            for encoded in base64_variants(&raw) {
                add_pattern(&mut patterns, encoded.to_vec());
                add_pattern(&mut patterns, percent_encode(&encoded, false));
                add_pattern(&mut patterns, percent_encode(&encoded, true));
            }
            add_pattern(&mut patterns, json_escape(&raw));
            if let Ok(text) = std::str::from_utf8(&raw) {
                if let Ok(quoted) = serde_json::to_string(text).map(Zeroizing::new)
                    && let Some(escaped) = quoted
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                {
                    add_pattern(&mut patterns, escaped.as_bytes().to_vec());
                }
                add_pattern(&mut patterns, unicode_escape(text, false));
                add_pattern(&mut patterns, unicode_escape(text, true));
            }
        }
        let longest = patterns
            .iter()
            .map(|pattern| pattern.len())
            .max()
            .unwrap_or(0);
        Ok(Self {
            values,
            patterns,
            longest,
        })
    }

    #[cfg(unix)]
    pub fn read_fd(fd: i32) -> Result<Zeroizing<Vec<u8>>, String> {
        use std::{fs::File, io::Read, os::fd::FromRawFd as _};

        // SAFETY: dup returns a new descriptor owned by this function. The
        // inherited descriptor remains owned by the benchmark supervisor.
        let duplicate = unsafe { libc::dup(fd) };
        if duplicate < 0 {
            return Err("cannot duplicate credential descriptor".to_owned());
        }
        // SAFETY: duplicate is a fresh, valid descriptor and File takes its
        // sole ownership.
        let file = unsafe { File::from_raw_fd(duplicate) };
        let mut value = Zeroizing::new(Vec::new());
        file.take((MAX_SECRET_BYTES + 1) as u64)
            .read_to_end(&mut value)
            .map_err(|_| "cannot read credential descriptor".to_owned())?;
        while value
            .last()
            .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
        {
            let _ = value.pop();
        }
        if value.is_empty() || value.len() > MAX_SECRET_BYTES {
            return Err("credential descriptor has an invalid payload length".to_owned());
        }
        Ok(value)
    }

    #[cfg(not(unix))]
    pub fn read_fd(_fd: i32) -> Result<Zeroizing<Vec<u8>>, String> {
        Err("credential descriptors require Unix".to_owned())
    }

    pub fn scanner(&self) -> SecretScanner<'_> {
        SecretScanner {
            secrets: self,
            tail: Zeroizing::new(Vec::new()),
        }
    }

    pub fn reject_public_values<'a>(
        &self,
        values: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), String> {
        for value in values {
            self.scanner().inspect(value.as_bytes())?;
        }
        Ok(())
    }

    pub fn value(&self, index: usize) -> Result<&[u8], String> {
        self.values
            .get(index)
            .map(|value| value.as_slice())
            .ok_or_else(|| "credential index is outside the secret set".to_owned())
    }
}

fn add_base64_patterns(patterns: &mut Vec<Zeroizing<Vec<u8>>>, value: &[u8]) {
    for encoded in base64_variants(value) {
        add_pattern(patterns, encoded.to_vec());
    }
}

fn base64_variants(value: &[u8]) -> [Zeroizing<Vec<u8>>; 4] {
    [
        Zeroizing::new(general_purpose::STANDARD.encode(value).into_bytes()),
        Zeroizing::new(general_purpose::STANDARD_NO_PAD.encode(value).into_bytes()),
        Zeroizing::new(general_purpose::URL_SAFE.encode(value).into_bytes()),
        Zeroizing::new(general_purpose::URL_SAFE_NO_PAD.encode(value).into_bytes()),
    ]
}

pub struct SecretScanner<'a> {
    secrets: &'a SecretSet,
    tail: Zeroizing<Vec<u8>>,
}

impl SecretScanner<'_> {
    pub fn inspect(&mut self, chunk: &[u8]) -> Result<(), String> {
        let mut joined = Zeroizing::new(Vec::with_capacity(self.tail.len() + chunk.len()));
        joined.extend_from_slice(&self.tail);
        joined.extend_from_slice(chunk);
        inspect_layers(self.secrets, &joined, 0, &mut 0usize)?;
        let retained = self
            .secrets
            .longest
            .saturating_add(4096)
            .min(MAX_DECODE_BYTES)
            .saturating_sub(1)
            .min(joined.len());
        self.tail.zeroize();
        self.tail.clear();
        self.tail
            .extend_from_slice(&joined[joined.len().saturating_sub(retained)..]);
        Ok(())
    }
}

fn inspect_layers(
    secrets: &SecretSet,
    bytes: &[u8],
    depth: usize,
    budget: &mut usize,
) -> Result<(), String> {
    *budget = budget
        .checked_add(bytes.len())
        .ok_or_else(|| "secret decode budget overflowed".to_owned())?;
    if *budget > MAX_DECODE_BYTES {
        return Err("secret decode budget exceeded".to_owned());
    }
    if secrets
        .patterns
        .iter()
        .any(|pattern| contains(bytes, pattern))
        || secrets.values.iter().any(|secret| contains(bytes, secret))
    {
        return Err("secret material appeared in child output".to_owned());
    }
    if depth >= MAX_DECODE_DEPTH {
        return Ok(());
    }
    let percent = Zeroizing::new(percent_decode(bytes));
    if percent.as_slice() != bytes {
        inspect_layers(secrets, &percent, depth + 1, budget)?;
    }
    for engine in [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(decoded) = engine.decode(bytes).map(Zeroizing::new)
            && !decoded.is_empty()
        {
            inspect_layers(secrets, &decoded, depth + 1, budget)?;
        }
    }
    if bytes.len() <= MAX_DECODE_BYTES
        && let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(bytes)
    {
        let result = inspect_json_strings(secrets, &value, depth + 1, budget);
        zeroize_json(&mut value);
        result?;
    }
    Ok(())
}

fn inspect_json_strings(
    secrets: &SecretSet,
    value: &serde_json::Value,
    depth: usize,
    budget: &mut usize,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(text) => inspect_layers(secrets, text.as_bytes(), depth, budget),
        serde_json::Value::Array(items) => {
            for item in items {
                inspect_json_strings(secrets, item, depth, budget)?;
            }
            Ok(())
        }
        serde_json::Value::Object(items) => {
            for (key, item) in items {
                inspect_layers(secrets, key.as_bytes(), depth, budget)?;
                inspect_json_strings(secrets, item, depth, budget)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn zeroize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => text.zeroize(),
        serde_json::Value::Array(items) => items.iter_mut().for_each(zeroize_json),
        serde_json::Value::Object(items) => items.values_mut().for_each(zeroize_json),
        _ => {}
    }
}

fn add_pattern(patterns: &mut Vec<Zeroizing<Vec<u8>>>, pattern: Vec<u8>) {
    if pattern.len() >= 4
        && !patterns
            .iter()
            .any(|existing| existing.as_slice() == pattern)
    {
        patterns.push(Zeroizing::new(pattern));
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn hex(value: &[u8], upper: bool) -> Vec<u8> {
    let alphabet = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut encoded = Vec::with_capacity(value.len().saturating_mul(2));
    for byte in value {
        encoded.push(alphabet[(byte >> 4) as usize]);
        encoded.push(alphabet[(byte & 0x0f) as usize]);
    }
    encoded
}

fn percent_encode(value: &[u8], upper: bool) -> Vec<u8> {
    let alphabet = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut encoded = Vec::with_capacity(value.len().saturating_mul(3));
    for byte in value {
        encoded.push(b'%');
        encoded.push(alphabet[(byte >> 4) as usize]);
        encoded.push(alphabet[(byte & 0x0f) as usize]);
    }
    encoded
}

fn json_escape(value: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(value.len().saturating_mul(6));
    for byte in value {
        if byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\') {
            encoded.push(*byte);
        } else {
            encoded.extend_from_slice(b"\\u00");
            encoded.extend_from_slice(&hex(&[*byte], false));
        }
    }
    encoded
}

fn unicode_escape(value: &str, upper: bool) -> Vec<u8> {
    let alphabet = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut encoded = Vec::with_capacity(value.len().saturating_mul(6));
    for unit in value.encode_utf16() {
        encoded.extend_from_slice(b"\\u");
        encoded.push(alphabet[((unit >> 12) & 0x0f) as usize]);
        encoded.push(alphabet[((unit >> 8) & 0x0f) as usize]);
        encoded.push(alphabet[((unit >> 4) & 0x0f) as usize]);
        encoded.push(alphabet[(unit & 0x0f) as usize]);
    }
    encoded
}

fn percent_decode(value: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0usize;
    while index < value.len() {
        if value[index] == b'%'
            && index.saturating_add(2) < value.len()
            && let (Some(high), Some(low)) =
                (hex_value(value[index + 1]), hex_value(value[index + 2]))
        {
            decoded.push((high << 4) | low);
            index = index.saturating_add(3);
        } else {
            decoded.push(value[index]);
            index = index.saturating_add(1);
        }
    }
    decoded
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_encoded_secret_split_across_chunks() {
        let secrets = SecretSet::from_values(vec![Zeroizing::new(b"open sesame".to_vec())])
            .expect("secret patterns");
        let encoded = general_purpose::STANDARD.encode(b"open sesame");
        let split = encoded.len() / 2;
        let mut scanner = secrets.scanner();
        scanner
            .inspect(&encoded.as_bytes()[..split])
            .expect("first half is incomplete");
        assert!(scanner.inspect(&encoded.as_bytes()[split..]).is_err());
    }

    #[test]
    fn accepts_unrelated_chunks() {
        let secrets = SecretSet::from_values(vec![Zeroizing::new(b"long-secret".to_vec())])
            .expect("secret patterns");
        let mut scanner = secrets.scanner();
        scanner.inspect(b"ordinary ").expect("ordinary prefix");
        scanner.inspect(b"diagnostic").expect("ordinary suffix");
    }

    #[test]
    fn rejects_short_secrets_instead_of_leaving_them_unscanned() {
        assert!(SecretSet::from_values(vec![Zeroizing::new(b"abc".to_vec())]).is_err());
    }

    #[test]
    fn detects_all_encodings_at_every_chunk_boundary() {
        let raw = b"q\"\\snow";
        let secrets =
            SecretSet::from_values(vec![Zeroizing::new(raw.to_vec())]).expect("secret patterns");
        let standard_json = serde_json::to_string(std::str::from_utf8(raw).expect("UTF-8 secret"))
            .expect("JSON escape");
        let standard_json = standard_json
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .expect("quoted JSON")
            .as_bytes()
            .to_vec();
        let mut mixed_percent = percent_encode(raw, false);
        for (index, byte) in mixed_percent.iter_mut().enumerate() {
            if index % 2 == 0 {
                byte.make_ascii_uppercase();
            }
        }
        let encodings = [
            raw.to_vec(),
            hex(raw, false),
            hex(raw, true),
            mixed_percent,
            general_purpose::STANDARD.encode(raw).into_bytes(),
            general_purpose::URL_SAFE_NO_PAD.encode(raw).into_bytes(),
            standard_json,
            unicode_escape(std::str::from_utf8(raw).expect("UTF-8 secret"), false),
        ];
        for encoded in encodings {
            for split in 0..=encoded.len() {
                let mut scanner = secrets.scanner();
                let first = scanner.inspect(&encoded[..split]);
                let detected = first.is_err() || scanner.inspect(&encoded[split..]).is_err();
                assert!(
                    detected,
                    "encoding of length {} escaped at split {split}",
                    encoded.len()
                );
            }
        }
    }

    #[test]
    fn detects_layered_and_hybrid_json_encodings_at_every_boundary() {
        let raw = b"layer-secret";
        let secrets = SecretSet::from_values(vec![Zeroizing::new(raw.to_vec())])
            .expect("layered secret patterns");
        let base = general_purpose::STANDARD.encode(raw);
        let percent_base = percent_encode(base.as_bytes(), false);
        let json_percent_base =
            serde_json::to_vec(std::str::from_utf8(&percent_base).expect("percent-base UTF-8"))
                .expect("layered JSON");
        let encodings = [
            percent_base,
            json_percent_base,
            br#""lay\u0065r-secret""#.to_vec(),
            br#"{"value":"lay\u0065r-secret"}"#.to_vec(),
        ];
        for encoded in encodings {
            for split in 0..=encoded.len() {
                let mut scanner = secrets.scanner();
                let first = scanner.inspect(&encoded[..split]);
                let detected = first.is_err() || scanner.inspect(&encoded[split..]).is_err();
                assert!(detected, "layered encoding escaped at split {split}");
            }
        }
    }

    #[test]
    fn detects_embedded_composite_encodings_at_every_boundary() {
        let raw = b"nested-secret";
        let secrets = SecretSet::from_values(vec![Zeroizing::new(raw.to_vec())])
            .expect("composite secret patterns");
        let percent = percent_encode(raw, false);
        let base_percent = general_purpose::STANDARD.encode(percent).into_bytes();
        let base = general_purpose::STANDARD.encode(raw);
        let percent_base = percent_encode(base.as_bytes(), true);
        for composite in [base_percent, percent_base] {
            let mut embedded = b"label=<".to_vec();
            embedded.extend_from_slice(&composite);
            embedded.extend_from_slice(b">;status=redacted");
            for split in 0..=embedded.len() {
                let mut scanner = secrets.scanner();
                let first = scanner.inspect(&embedded[..split]);
                let detected = first.is_err() || scanner.inspect(&embedded[split..]).is_err();
                assert!(detected, "embedded composite escaped at split {split}");
            }
        }
    }
}
