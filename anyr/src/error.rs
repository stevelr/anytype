/*
 * anyr - list, search, and manipulate anytype objects
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */
//! Exit codes and user-facing rendering for CLI failures.
//!
//! [`AnytypeError`] deliberately redacts its `Display` output: the same error
//! type is returned to the MCP server, where free-form upstream text and
//! request content must never reach a shared transcript. Every redacted value
//! is still retained in the variant's fields for callers that need it, and the
//! CLI is such a caller — it runs interactively, on the user's own machine,
//! against the user's own data, and prints to their terminal. [`render`]
//! therefore re-expands those fields into a message that names what failed.

use std::fmt::Write as _;

use anyhow::Error;
use anytype::prelude::{AnytypeError, ResolveCandidate};

/// Maximum characters of server- or user-supplied text included in a message.
const MAX_DETAIL_CHARS: usize = 1024;

pub fn exit_code(err: &Error) -> i32 {
    if matches!(
        err.downcast_ref::<AnytypeError>(),
        Some(
            AnytypeError::Unauthorized
                | AnytypeError::NoKeyStore
                | AnytypeError::KeyStore { .. }
                | AnytypeError::Auth { .. }
        )
    ) {
        return 2;
    }
    1
}

/// Renders a CLI failure as the descriptive message shown to the user.
///
/// Any [`anyhow`] context layers are kept and joined outermost-first, so a
/// wrapped cause is no longer silently dropped. When the root cause is an
/// [`AnytypeError`], its redacted `Display` is replaced by [`describe`].
pub fn render(err: &Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in err.chain() {
        if let Some(error) = cause.downcast_ref::<AnytypeError>() {
            parts.push(describe(error));
            break;
        }
        parts.push(cause.to_string());
    }
    parts.join(": ")
}

/// Expands one [`AnytypeError`] into a message naming the failing item.
///
/// The match is exhaustive so a new variant is a compile error here rather
/// than a silently redacted message.
#[allow(clippy::too_many_lines)] // one arm per error variant
fn describe(error: &AnytypeError) -> String {
    match error {
        AnytypeError::NotFound { obj_type, key } => {
            let mut message = format!("{} \"{}\" was not found", noun(obj_type), detail(key));
            if let Some(hint) = lookup_hint(obj_type) {
                write_hint(&mut message, hint);
            }
            message
        }

        AnytypeError::Ambiguous {
            obj_type,
            key,
            candidates,
        } => {
            let mut message = format!(
                "{} \"{}\" matches more than one item; retry with an id",
                noun(obj_type),
                detail(key)
            );
            write_candidates(&mut message, candidates);
            message
        }

        AnytypeError::ResolutionLimitExceeded {
            obj_type,
            key,
            limit,
        } => format!(
            "could not resolve {} \"{}\" within the {limit}-item scan limit; retry with an id",
            noun(obj_type),
            detail(key)
        ),

        AnytypeError::Validation { message } => format!("invalid input: {}", detail(message)),

        AnytypeError::Auth { message } => {
            format!("authentication failed: {}", detail(message))
        }

        AnytypeError::GrpcUnavailable { message } => {
            format!("gRPC service unavailable: {}", detail(message))
        }

        AnytypeError::Grpc { source } => format!("gRPC error: {source}"),

        AnytypeError::KeyStore { source } => format!("keystore error: {source}"),

        AnytypeError::Other { message } => detail(message),

        // The `url` fields of these two variants are the raw request targets,
        // which can carry userinfo or a query-string token, so the request is
        // named from the bounded, credential-free `diagnostic()` view instead.
        AnytypeError::ApiError { code, message, .. } => format!(
            "Anytype API error {code} for {}: {}",
            request_target(error),
            detail(message)
        ),

        AnytypeError::Http { source, .. } => {
            let mut message = format!("HTTP transport error for {}", request_target(error));
            if let Some(cause) = transport_cause(source) {
                let _ = write!(message, ": {}", detail(&cause));
            }
            message
        }

        AnytypeError::RateLimitExceeded { header, duration } => format!(
            "rate limit exceeded; waited {} secs (server retry-after: {})",
            duration.as_secs(),
            detail(header)
        ),

        AnytypeError::VerifyTimeout {
            obj_type,
            key,
            attempts,
            timeout,
            last_error,
        } => {
            let mut message = format!(
                "{} \"{}\" was not confirmed after {attempts} attempts in {timeout:?}",
                noun(obj_type),
                detail(key)
            );
            if let Some(last) = last_error {
                let _ = write!(message, "; last error: {}", detail(last));
            }
            message
        }

        AnytypeError::BodyGraph {
            object_id,
            kind,
            detail: context,
        } => format!(
            "body of object {object_id} failed graph validation: {kind} ({})",
            detail(context)
        ),

        AnytypeError::BodyMutationIndeterminate {
            object_id,
            block_id,
            attempts,
            timeout,
            ..
        } => {
            let mut message = format!("body change to object {object_id}");
            if let Some(block_id) = block_id {
                let _ = write!(message, " (block {block_id})");
            }
            let _ = write!(
                message,
                " could not be confirmed after {attempts} attempts in {timeout:?}; \
                 re-read the body before retrying"
            );
            message
        }

        // Remaining variants already render every value they retain.
        AnytypeError::HttpTimeout { .. }
        | AnytypeError::HttpMutationIndeterminate { .. }
        | AnytypeError::ResponseTooLarge { .. }
        | AnytypeError::FileHeaderEvidenceTooLarge { .. }
        | AnytypeError::InvalidFileResponseHeader { .. }
        | AnytypeError::ChatSseEventTooLarge { .. }
        | AnytypeError::ChatSseTransport { .. }
        | AnytypeError::ChatTimestamp { .. }
        | AnytypeError::ChatHistoryEvidence { .. }
        | AnytypeError::ChatEditTimestampNotAdvanced
        | AnytypeError::TooManyRetries { .. }
        | AnytypeError::Deserialization { .. }
        | AnytypeError::Serialization { .. }
        | AnytypeError::Unauthorized
        | AnytypeError::Forbidden
        | AnytypeError::NoKeyStore
        | AnytypeError::CacheDisabled
        | AnytypeError::BodyRpcLifecycle { .. }
        | AnytypeError::CollectionMembershipEvidence { .. }
        | AnytypeError::TypePropertyClassification { .. }
        | AnytypeError::AttachedDiscussion { .. } => error.to_string(),
    }
}

/// Normalizes a resolver's item kind for use inside a sentence.
///
/// Constructors spell these inconsistently (`"Space"`, `"space"`, `"Tag"`,
/// `"body block"`), so they are lowercased for a uniform message.
fn noun(obj_type: &str) -> String {
    obj_type.to_lowercase()
}

/// Returns a CLI command that lists the items of `obj_type`, when one exists.
///
/// Each hint uses the global `-t` flag so the suggested command prints a
/// readable table rather than the default JSON.
fn lookup_hint(obj_type: &str) -> Option<&'static str> {
    match noun(obj_type).as_str() {
        "space" => Some("run `anyr space list -t` to see the spaces you can access"),
        "type" => Some("run `anyr type list <space> -t` to see the types in that space"),
        "property" => Some("run `anyr property list <space> -t` to see the available properties"),
        "tag" => Some("run `anyr tag list <space> <property> -t` to see the available tags"),
        "chat" => Some("run `anyr chat list --space <space> -t` to see the chats in that space"),
        "template" => Some("run `anyr template list <space> <type> -t` to see the templates"),
        _ => None,
    }
}

/// Bounds and escapes server- or user-supplied text for terminal output.
///
/// Control characters are escaped so a malformed name or upstream payload
/// cannot emit terminal escape sequences, and the length is capped so a large
/// response body cannot flood the terminal.
fn detail(value: &str) -> String {
    let mut shown = String::new();
    for ch in value.chars().take(MAX_DETAIL_CHARS) {
        // Newlines are kept: multi-line upstream payloads stay readable.
        if ch.is_control() && ch != '\n' {
            shown.extend(ch.escape_default());
        } else {
            shown.push(ch);
        }
    }
    if value.chars().count() > MAX_DETAIL_CHARS {
        shown.push('…');
    }
    shown
}

/// Names the failed request as `METHOD /path`.
///
/// [`AnytypeError::diagnostic`] is the accessor built for this: it reduces the
/// retained request target to a bounded path with no authority, query, or
/// fragment, so no credential carried in a URL can reach the terminal.
fn request_target(error: &AnytypeError) -> String {
    let diagnostic = error.diagnostic();
    format!(
        "{} {}",
        diagnostic.method.as_deref().unwrap_or("unknown"),
        diagnostic.path.as_deref().unwrap_or("unknown")
    )
}

/// Returns the innermost cause of a transport failure, such as
/// `Connection refused (os error 111)`.
///
/// `reqwest`'s own `Display` embeds the request URL, which is why the library
/// drops it; the leaf of its source chain is the underlying I/O or protocol
/// error, which names the real problem without a credential-bearing target.
///
/// Kept generic so `anyr` does not take a direct `reqwest` dependency purely
/// to name the argument type.
fn transport_cause(source: &(impl std::error::Error + 'static)) -> Option<String> {
    let mut leaf: Option<&(dyn std::error::Error + 'static)> = None;
    let mut current = std::error::Error::source(source);
    while let Some(cause) = current {
        leaf = Some(cause);
        current = cause.source();
    }
    leaf.map(ToString::to_string)
}

fn write_hint(message: &mut String, hint: &str) {
    let _ = write!(message, "\n  hint: {hint}");
}

fn write_candidates(message: &mut String, candidates: &[ResolveCandidate]) {
    if candidates.is_empty() {
        return;
    }
    message.push_str("\n  candidates:");
    for candidate in candidates {
        let _ = write!(
            message,
            "\n    {} ({})",
            detail(candidate.name()),
            candidate.id()
        );
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{describe, exit_code, render};
    use anytype::prelude::{AnytypeError, ResolveCandidate};

    #[test]
    fn missing_space_names_the_space_and_suggests_a_lookup() {
        let message = describe(&AnytypeError::NotFound {
            obj_type: "space".to_owned(),
            key: "MyChats".to_owned(),
        });
        assert!(
            message.starts_with("space \"MyChats\" was not found"),
            "unexpected message: {message}"
        );
        assert!(message.contains("anyr space list -t"), "missing hint");
    }

    #[test]
    fn api_error_surfaces_the_response_body_without_the_credential_bearing_url() {
        let message = describe(&AnytypeError::ApiError {
            code: 500,
            method: "GET".to_owned(),
            url: "https://user:PASSWORD@anytype.invalid/v1/objects?token=QUERY_SECRET".to_owned(),
            message: r#"{"code":"internal_server_error"}"#.to_owned(),
        });
        assert!(
            message.starts_with("Anytype API error 500 for GET /v1/objects"),
            "unexpected message: {message}"
        );
        assert!(message.contains("internal_server_error"), "{message}");
        assert!(!message.contains("PASSWORD"), "leaked userinfo: {message}");
        assert!(
            !message.contains("QUERY_SECRET"),
            "leaked query token: {message}"
        );
    }

    #[test]
    fn every_lookup_hint_suggests_table_output() {
        for obj_type in ["space", "type", "property", "tag", "chat", "template"] {
            let hint = super::lookup_hint(obj_type).expect("kind has a hint");
            assert!(hint.contains(" -t"), "{obj_type} hint omits -t: {hint}");
        }
    }

    #[test]
    fn inconsistent_kind_spelling_is_normalized() {
        let message = describe(&AnytypeError::NotFound {
            obj_type: "Tag".to_owned(),
            key: "urgent".to_owned(),
        });
        assert!(
            message.starts_with("tag \"urgent\" was not found"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn invalid_object_id_reports_the_rejected_value() {
        let error = anytype::validation::ValidationLimits::default()
            .validate_id("page", "object_id")
            .expect_err("\"page\" is not an object id");
        let message = describe(&error);
        assert!(
            message.contains("\"page\"") && message.contains("not a valid object id"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn ambiguous_name_lists_its_candidates() {
        let message = describe(&AnytypeError::Ambiguous {
            obj_type: "space".to_owned(),
            key: "Work".to_owned(),
            candidates: vec![
                ResolveCandidate::new("bafyrei-one", "Work"),
                ResolveCandidate::new("bafyrei-two", "Work"),
            ],
        });
        assert!(message.contains("matches more than one item"));
        assert!(message.contains("bafyrei-one") && message.contains("bafyrei-two"));
    }

    #[test]
    fn anyhow_context_is_kept_above_the_expanded_cause() {
        let error = anyhow::Error::from(AnytypeError::NotFound {
            obj_type: "space".to_owned(),
            key: "MyChats".to_owned(),
        })
        .context("listing types");
        let message = render(&error);
        assert!(
            message.starts_with("listing types: space \"MyChats\" was not found"),
            "unexpected message: {message}"
        );
    }

    #[test]
    fn non_anytype_errors_keep_their_full_context_chain() {
        let error = anyhow::anyhow!("root cause").context("outer step");
        assert_eq!(render(&error), "outer step: root cause");
    }

    #[test]
    fn control_characters_in_supplied_text_are_escaped() {
        let message = describe(&AnytypeError::NotFound {
            obj_type: "space".to_owned(),
            key: "esc\u{1b}[31mred".to_owned(),
        });
        assert!(!message.contains('\u{1b}'), "unescaped escape: {message}");
    }

    #[test]
    fn long_supplied_text_is_truncated() {
        let message = describe(&AnytypeError::Other {
            message: "x".repeat(super::MAX_DETAIL_CHARS + 100),
        });
        assert!(message.chars().count() <= super::MAX_DETAIL_CHARS + 1);
        assert!(message.ends_with('…'));
    }

    #[test]
    fn verify_timeout_names_the_item_and_last_error() {
        let message = describe(&AnytypeError::VerifyTimeout {
            obj_type: "object".to_owned(),
            key: "Notes".to_owned(),
            attempts: 3,
            timeout: Duration::from_secs(2),
            last_error: Some("still missing".to_owned()),
        });
        assert!(message.contains("object \"Notes\""), "{message}");
        assert!(message.contains("still missing"), "{message}");
    }

    #[test]
    fn authentication_failures_keep_their_dedicated_exit_code() {
        let error = anyhow::Error::from(AnytypeError::Unauthorized);
        assert_eq!(exit_code(&error), 2);
        assert_eq!(exit_code(&anyhow::anyhow!("other")), 1);
    }
}
