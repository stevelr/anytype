// SPDX-FileCopyrightText: 2025-2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Shared gRPC helpers used across the crate.

use anytype_rpc::auth::with_token;
use tonic::Request;

use crate::{Result, error::AnytypeError};

/// Trait for gRPC response error types with `code` and `description` fields.
pub(crate) trait GrpcError {
    fn code(&self) -> i32;
    fn description(&self) -> &str;
}

/// Check a gRPC response error field, returning `Err` if the code is non-zero.
pub(crate) fn ensure_error_ok<T: GrpcError>(error: Option<&T>, action: &str) -> Result<()> {
    if let Some(error) = error
        && error.code() != 0
    {
        return Err(AnytypeError::Other {
            message: format!(
                "{action} failed: {} (code {})",
                error.description(),
                error.code()
            ),
        });
    }
    Ok(())
}

/// Convert a tonic status into an [`AnytypeError`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn grpc_status(status: tonic::Status) -> AnytypeError {
    AnytypeError::Other {
        message: format!("gRPC request failed: {status}"),
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
                fn description(&self) -> &str { &self.description }
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
use anytype_rpc::anytype::rpc::object::{list_delete, search_with_meta};

impl_grpc_error!(
    list_delete::response::Error,
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
