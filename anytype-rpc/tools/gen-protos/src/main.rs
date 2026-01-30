//!
//! gen-protos
//!
//! Download .proto files from anytype-heart repo and generate rust sources in src/gen
//!
//! Usage:
//!
//! ```sh
//!   #!/usr/bin/env bash
//!  ref=${1:-develop}   # git branch, tag, or revision. Defaults to 'develop'
//!  set -euo pipefail
//!  tmp_dir="$(mktemp -d)"
//!  trap 'rm -rf "$tmp_dir"' EXIT
//!  curl -fsSL "https://codeload.github.com/anyproto/anytype-heart/tar.gz/${ref}" -o "$tmp_dir/repo.tgz"
//!  tar -xzf "$tmp_dir/repo.tgz" -C "$tmp_dir"
//!  repo_dir="$(find "$tmp_dir" -maxdepth 1 -type d -name "anytype-heart-*" | head -n 1)"
//!  if [[ -z "$repo_dir" ]]; then
//!      echo "Failed to locate extracted repo directory" >&2
//!      exit 1
//!  fi
//!  CARGO_TARGET_DIR=./tools/gen-protos/target cargo run --manifest-path tools/gen-protos/Cargo.toml -- "$repo_dir"
//!  rustfmt --edition 2024 src/gen/*.rs
//! ```
//!

use std::fs;
use std::path::PathBuf;
use tonic_prost_build::configure;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source_root = std::env::args().nth(1).ok_or(
        "usage: gen-protos <path-to-anytype-heart-root>\n\
        (expects pkg/lib/pb/model/protos and pb/protos in that root)",
    )?;
    let source_root = PathBuf::from(source_root);

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or("unable to resolve repo root")?
        .to_path_buf();

    let out_dir = repo_root.join("src/gen");
    fs::create_dir_all(&out_dir)?;

    let tmp_dir = std::env::temp_dir().join(format!("anytype-rpc-protos-{}", std::process::id()));
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)?;
    }
    fs::create_dir_all(&tmp_dir)?;

    let model_dir = tmp_dir.join("pkg/lib/pb/model/protos");
    fs::create_dir_all(&model_dir)?;

    // Pre-process: Fix naming conflicts in models.proto
    let models_proto =
        fs::read_to_string(source_root.join("pkg/lib/pb/model/protos/models.proto"))?;
    let models_proto = models_proto
        // In Block message: rename `oneof content` to `oneof content_value`
        .replace("oneof content {", "oneof content_value {")
        // In Metadata message: rename `oneof payload` to `oneof payload_value`
        .replace(
            "oneof payload {\n        Payload.IdentityPayload",
            "oneof payload_value {\n        Payload.IdentityPayload",
        );
    fs::write(model_dir.join("models.proto"), models_proto)?;

    fs::copy(
        source_root.join("pkg/lib/pb/model/protos/localstore.proto"),
        model_dir.join("localstore.proto"),
    )?;

    // Stage 1: Compile model protos
    configure()
        .build_client(false)
        .build_server(false)
        .out_dir(&out_dir)
        .compile_protos(
            &[
                model_dir.join("models.proto").to_str().unwrap(),
                model_dir.join("localstore.proto").to_str().unwrap(),
            ],
            &[tmp_dir.to_str().unwrap()],
        )?;

    // Stage 2: Compile storage protos
    configure()
        .build_client(false)
        .build_server(false)
        .out_dir(&out_dir)
        .compile_protos(
            &[source_root
                .join("pkg/lib/pb/storage/protos/file.proto")
                .to_str()
                .unwrap()],
            &[
                source_root.to_str().unwrap(),
                source_root.join("pb").to_str().unwrap(),
            ],
        )?;

    // Stage 3: Compile service protos, referencing model types generated above
    configure()
        .build_client(true)
        .build_server(false)
        .out_dir(&out_dir)
        .extern_path(".anytype.model", "crate::model")
        .compile_protos(
            &[
                source_root
                    .join("pb/protos/service/service.proto")
                    .to_str()
                    .unwrap(),
                source_root
                    .join("pb/protos/commands.proto")
                    .to_str()
                    .unwrap(),
                source_root.join("pb/protos/events.proto").to_str().unwrap(),
                source_root
                    .join("pb/protos/snapshot.proto")
                    .to_str()
                    .unwrap(),
                source_root
                    .join("pb/protos/changes.proto")
                    .to_str()
                    .unwrap(),
            ],
            &[
                source_root.to_str().unwrap(),
                source_root.join("pb").to_str().unwrap(),
            ],
        )?;

    fix_doc_list_indents(&out_dir)?;
    fs::remove_dir_all(&tmp_dir)?;
    Ok(())
}

/// cargo clippy complains about indented lists inside comments. This fixes that
fn fix_doc_list_indents(out_dir: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(out_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let mut changed = false;
        let mut out = String::with_capacity(text.len());
        for line in text.lines() {
            if let Some(new_line) = indent_doc_list_line(line) {
                out.push_str(&new_line);
                changed = true;
            } else if let Some(new_line) = indent_doc_list_continuation(line) {
                out.push_str(&new_line);
                changed = true;
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        if changed {
            fs::write(&path, out)?;
        }
    }
    Ok(())
}

fn indent_doc_list_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("/// ") {
        return None;
    }
    let prefix_len = line.len() - trimmed.len();
    let content = &trimmed[4..];
    if content.starts_with(' ') {
        return None;
    }
    let mut chars = content.chars();
    let mut digit_count = 0;
    let mut has_letter = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            digit_count += 1;
            continue;
        }
        if digit_count == 0 {
            return None;
        }
        if c.is_ascii_alphabetic() {
            has_letter = true;
            if let Some(next) = chars.next() {
                if next != '.' {
                    return None;
                }
            } else {
                return None;
            }
        } else if c != '.' {
            return None;
        }
        break;
    }
    let indent = " ".repeat(prefix_len);
    let list_indent = if has_letter { "       " } else { "    " };
    Some(format!("{indent}///{list_indent}{content}"))
}

fn indent_doc_list_continuation(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("///    ") {
        return None;
    }
    let content = &trimmed[7..];
    if content.starts_with(' ') {
        return None;
    }
    let prefix_len = line.len() - trimmed.len();
    let indent = " ".repeat(prefix_len);
    Some(format!("{indent}///       {content}"))
}
