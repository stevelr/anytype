// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared gRPC helpers used across the crate.

use anytype_rpc::{
    auth::with_token,
    deadline::{GrpcDeadlineError, GrpcTimeoutClass, GrpcTimeoutOutcome},
    error::AnytypeGrpcError,
};
use tonic::Request;

use crate::{Result, error::AnytypeError};

/// Trait for gRPC response error types with `code` and `description` fields.
pub(crate) trait GrpcError {
    fn code(&self) -> i32;
}

/// Check a gRPC response error field, returning `Err` if the code is non-zero.
pub(crate) fn ensure_error_ok<T: GrpcError>(error: Option<&T>, action: &str) -> Result<()> {
    if let Some(error) = error
        && error.code() != 0
    {
        return Err(AnytypeError::Other {
            message: format!("{action} failed with fixed code {}", error.code()),
        });
    }
    Ok(())
}

/// Convert a tonic status into an [`AnytypeError`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn grpc_status(status: tonic::Status) -> AnytypeError {
    grpc_status_for(
        status,
        GrpcTimeoutClass::OrdinaryUnary,
        GrpcTimeoutOutcome::MutationIndeterminate,
        std::time::Duration::ZERO,
    )
}

/// Convert a tonic status with explicit deadline semantics.
pub(crate) fn grpc_status_for(
    status: tonic::Status,
    class: GrpcTimeoutClass,
    outcome: GrpcTimeoutOutcome,
    elapsed: std::time::Duration,
) -> AnytypeError {
    match status.code() {
        tonic::Code::Unauthenticated => return AnytypeError::Unauthorized,
        tonic::Code::PermissionDenied => return AnytypeError::Forbidden,
        _ => {}
    }
    if let Some(source) = GrpcDeadlineError::from_status(&status, class, outcome, elapsed) {
        return AnytypeError::Grpc {
            source: AnytypeGrpcError::Deadline { source },
        };
    }
    AnytypeError::Other {
        message: format!("gRPC request failed with code {}", status.code()),
    }
}

/// Attach a bearer token to a tonic request.
pub(crate) fn with_token_request<T>(request: Request<T>, token: &str) -> Result<Request<T>> {
    with_token(request, token).map_err(|err| AnytypeError::Auth {
        message: err.to_string(),
    })
}

// ---------------------------------------------------------------------------
// GrpcError impls for every response-error type used in the crate
// ---------------------------------------------------------------------------

macro_rules! impl_grpc_error {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl GrpcError for $ty {
                fn code(&self) -> i32 { self.code }
            }
        )+
    };
}

// chat
use anytype_rpc::anytype::rpc::chat::{
    add_message, delete_message, edit_message_content, get_messages, get_messages_by_ids, read_all,
    read_messages, subscribe_last_messages, subscribe_to_message_previews, toggle_message_reaction,
    unread, unsubscribe, unsubscribe_from_message_previews,
};

impl_grpc_error!(
    add_message::response::Error,
    delete_message::response::Error,
    edit_message_content::response::Error,
    get_messages::response::Error,
    get_messages_by_ids::response::Error,
    read_all::response::Error,
    read_messages::response::Error,
    subscribe_last_messages::response::Error,
    subscribe_to_message_previews::response::Error,
    toggle_message_reaction::response::Error,
    unread::response::Error,
    unsubscribe::response::Error,
    unsubscribe_from_message_previews::response::Error,
);

// file
use anytype_rpc::anytype::rpc::file::{discard_preload, download, upload};

impl_grpc_error!(
    discard_preload::response::Error,
    download::response::Error,
    upload::response::Error,
);

// object
use anytype_rpc::anytype::rpc::object::{
    close as object_close, discussion_add, list_delete, search_subscribe, search_unsubscribe,
    search_with_meta, show as object_show,
};

impl_grpc_error!(
    list_delete::response::Error,
    object_close::response::Error,
    discussion_add::response::Error,
    object_show::response::Error,
    search_subscribe::response::Error,
    search_unsubscribe::response::Error,
    search_with_meta::response::Error,
);

// process
use anytype_rpc::anytype::rpc::process::{
    subscribe as process_subscribe, unsubscribe as process_unsubscribe,
};

impl_grpc_error!(
    process_subscribe::response::Error,
    process_unsubscribe::response::Error,
);

// workspace
use anytype_rpc::anytype::rpc::workspace::open as workspace_open;

impl_grpc_error!(workspace_open::response::Error);

// body-block mutations
use anytype_rpc::anytype::rpc::{
    block::{
        create as block_create, list_delete as block_list_delete, list_move_to_existing_object,
        list_set_align, list_set_background_color, list_set_vertical_align,
    },
    block_div, block_latex, block_link, block_table, block_text,
};

impl_grpc_error!(
    block_create::response::Error,
    block_list_delete::response::Error,
    list_move_to_existing_object::response::Error,
    list_set_align::response::Error,
    list_set_background_color::response::Error,
    list_set_vertical_align::response::Error,
    block_text::set_text::response::Error,
    block_text::set_color::response::Error,
    block_text::set_style::response::Error,
    block_text::set_checked::response::Error,
    block_text::set_icon::response::Error,
    block_latex::set_text::response::Error,
    block_table::create::response::Error,
    block_div::list_set_style::response::Error,
    block_link::list_set_appearance::response::Error,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_status_preserves_authentication_classification_without_payloads() {
        let unauthenticated = grpc_status(tonic::Status::unauthenticated("SECRET_TOKEN"));
        let permission_denied = grpc_status(tonic::Status::permission_denied("SECRET_SCOPE"));

        assert!(matches!(&unauthenticated, AnytypeError::Unauthorized));
        assert!(matches!(&permission_denied, AnytypeError::Forbidden));
        assert_eq!(
            unauthenticated.grpc_admission_failure(),
            crate::error::GrpcAdmissionFailure::Authentication
        );
        assert_eq!(
            permission_denied.grpc_admission_failure(),
            crate::error::GrpcAdmissionFailure::Authentication
        );
        assert!(!format!("{unauthenticated:?}").contains("SECRET_TOKEN"));
        assert!(!format!("{permission_denied:?}").contains("SECRET_SCOPE"));
    }
}
