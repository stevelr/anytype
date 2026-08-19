//! # Name and id resolution
//!
//! Anytype apis take ids (spaces, types, views, properties, chats), but
//! people and tools usually start from a name or key. The helpers in this
//! module accept an "id or name" string and resolve it to the id (or full
//! item), so callers can offer the friendlier form everywhere:
//!
//! - [`resolve_space_id`](AnytypeClient::resolve_space_id) - space name or id → space id
//! - [`resolve_type`](AnytypeClient::resolve_type) - type key, name, or id → [`Type`]
//! - [`resolve_type_id`](AnytypeClient::resolve_type_id) - type key, name, or id → type id
//! - [`resolve_type_ids`](AnytypeClient::resolve_type_ids) - batch form of [`resolve_type_id`](AnytypeClient::resolve_type_id)
//! - [`resolve_type_key`](AnytypeClient::resolve_type_key) - type key, name, or id → type key
//! - [`resolve_template`](AnytypeClient::resolve_template) - template id or unique name → verified template
//! - [`resolve_view_id`](AnytypeClient::resolve_view_id) - view name or id → view id
//! - [`resolve_property_id`](AnytypeClient::resolve_property_id) - property key or id → property id
//! - [`resolve_chat_target`](AnytypeClient::resolve_chat_target) - chat (or space) name or id → [`ChatTarget`]
//! - [`resolve_chat_ids`](AnytypeClient::resolve_chat_ids) - batch form of [`resolve_chat_target`](AnytypeClient::resolve_chat_target)
//! - [`resolve_chat_name`](AnytypeClient::resolve_chat_name) - chat id → display name
//! - [`resolve_message_id`](AnytypeClient::resolve_message_id) - message id or chat order id → message id
//! - [`resolve_message_ids`](AnytypeClient::resolve_message_ids) - batch form of [`resolve_message_id`](AnytypeClient::resolve_message_id)
//!
//! Shared conventions:
//!
//! - A value that already looks like an object id ([`looks_like_object_id`]) is
//!   usually passed through without a server round trip. Resolvers that must
//!   return type metadata perform one cache-independent scoped type GET and
//!   require the returned type ID to match exactly.
//! - For types, a leading `@` forces key interpretation (`@page` means the
//!   type with key `page`), and a value starting with an uppercase ascii
//!   letter is matched case-insensitively against type *names*.
//! - Name matches are case-insensitive and must be unique:
//!   no match returns [`AnytypeError::NotFound`], more than one match
//!   returns [`AnytypeError::Ambiguous`].
//! - Name scans examine at most [`MAX_RESOLVE_SCAN_ITEMS`] upstream rows.
//!   If uniqueness cannot be proved within that bound, they return
//!   [`AnytypeError::ResolutionLimitExceeded`] instead of guessing.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use anytype::prelude::*;
//! # use anytype::Result;
//! # async fn example(client: &AnytypeClient) -> Result<()> {
//! let space_id = client.resolve_space_id("Work").await?;
//! let type_key = client.resolve_type_key(&space_id, "Task").await?;
//! let objects = client.objects(&space_id).list().await?;
//! # Ok(())
//! # }
//! ```

use std::collections::HashSet;

use futures::StreamExt;

use crate::{
    Result, client::AnytypeClient, error::AnytypeError, objects::Object, types::Type,
    validation::looks_like_object_id,
};

/// Maximum number of alternatives retained for an ambiguous resolution.
pub const MAX_RESOLVE_CANDIDATES: usize = 10;
/// Maximum number of upstream rows a name resolver examines before refusing
/// to guess from an incomplete scan.
pub const MAX_RESOLVE_SCAN_ITEMS: usize = 1_000;
/// Maximum number of characters retained in a resolver candidate name.
pub const MAX_RESOLVE_CANDIDATE_NAME_CHARS: usize = 256;

const MAX_RESOLVE_CANDIDATE_ID_CHARS: usize = 256;

// An explicit limit bypasses cache-prime shortcuts. Choosing 99 also makes
// short-page completion unambiguous around the 1,000-row scan ceiling and for
// chat searches, whose gRPC response has no total/has-more metadata.
pub(crate) const RESOLVE_PAGE_SIZE: u32 = 99;

/// Name of the default chat created in every space.
///
/// [`resolve_chat_target`](AnytypeClient::resolve_chat_target) falls back to
/// the chat with this name when given only a space.
pub const DEFAULT_CHAT_NAME: &str = "General";

/// One safe, bounded-list alternative for an ambiguous resolver lookup.
///
/// The identifier and display name remain ordinary strings in the API client;
/// protocol adapters must validate them for their own wire constraints before
/// exposing them. Resolver errors cap the containing list at
/// [`MAX_RESOLVE_CANDIDATES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveCandidate {
    id: String,
    name: String,
}

impl ResolveCandidate {
    /// Creates a resolver alternative from its stable id and display name.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }

    /// Borrows the stable identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Borrows the display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// A resolved chat reference: the chat object id, plus the containing
/// space id when it could be determined.
///
/// Returned by [`resolve_chat_target`](AnytypeClient::resolve_chat_target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatTarget {
    /// id of the space containing the chat, when known
    pub space_id: Option<String>,
    /// chat object id
    pub chat_id: String,
}

impl AnytypeClient {
    /// Resolves a space name or id into a space id.
    ///
    /// An id is returned unchanged. A name is matched case-insensitively
    /// against the names of all spaces the user can access.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if no space has that name
    /// - [`AnytypeError::Ambiguous`] if more than one space has that name
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # async fn example(client: &AnytypeClient) -> Result<(), AnytypeError> {
    /// let space_id = client.resolve_space_id("Work").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn resolve_space_id(&self, space_id_or_name: &str) -> Result<String> {
        self.resolve_space_id_with_page_limit(space_id_or_name, None)
            .await
    }

    /// Resolves a space name or ID while bounding each resolver page body.
    ///
    /// Stable IDs return without I/O. A nonzero `page_response_limit` applies
    /// independently to every physical page response used by name resolution.
    pub async fn resolve_space_id_bounded(
        &self,
        space_id_or_name: &str,
        page_response_limit: u64,
    ) -> Result<String> {
        if page_response_limit == 0 {
            return Err(AnytypeError::Validation {
                message: "space resolver page response limit must be nonzero".to_owned(),
            });
        }
        self.resolve_space_id_with_page_limit(space_id_or_name, Some(page_response_limit))
            .await
    }

    async fn resolve_space_id_with_page_limit(
        &self,
        space_id_or_name: &str,
        page_response_limit: Option<u64>,
    ) -> Result<String> {
        if looks_like_object_id(space_id_or_name) {
            return Ok(space_id_or_name.to_string());
        }

        let needle = space_id_or_name.to_lowercase();
        let mut request = self.spaces().limit(RESOLVE_PAGE_SIZE);
        if let Some(limit) = page_response_limit {
            request = request.response_limit_bytes(limit);
        }
        let matches = scan_paged_matches(
            request.list().await?,
            "space",
            space_id_or_name,
            |space| space.name.to_lowercase() == needle,
            |space| ResolveCandidate::new(&space.id, &space.name),
        )
        .await?;

        match matches {
            MatchClassification::None => Err(not_found("space", space_id_or_name)),
            MatchClassification::Unique(space) => Ok(space.id),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("space", space_id_or_name, candidates))
            }
        }
    }

    /// Resolves a type key, name, or id into the full [`Type`].
    ///
    /// Accepts `@key` (explicit key), a type id, a Name (uppercase first
    /// letter, matched case-insensitively against type names), or a key.
    /// An explicit type id performs exactly one cache-independent scoped GET;
    /// it never primes the all-types cache and rejects a mismatched response.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if nothing matches
    /// - [`AnytypeError::Ambiguous`] if more than one type matches
    pub async fn resolve_type(&self, space_id: &str, type_key_or_id: &str) -> Result<Type> {
        if let Some(stripped) = type_key_or_id.strip_prefix('@') {
            return match self
                .scan_type_matches(space_id, stripped, TypeMatchMode::Key)
                .await?
            {
                MatchClassification::None => Err(not_found("type", stripped)),
                MatchClassification::Unique(typ) => Ok(typ),
                MatchClassification::Ambiguous(candidates) => {
                    Err(ambiguous("type", stripped, candidates))
                }
            };
        }
        if looks_like_object_id(type_key_or_id) {
            return self.get_type(space_id, type_key_or_id).get_direct().await;
        }
        if starts_with_uppercase(type_key_or_id) {
            return self.resolve_type_by_name(space_id, type_key_or_id).await;
        }
        match self
            .scan_type_matches(space_id, type_key_or_id, TypeMatchMode::Any)
            .await?
        {
            MatchClassification::None => Err(not_found("type", type_key_or_id)),
            MatchClassification::Unique(typ) => Ok(typ),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("type", type_key_or_id, candidates))
            }
        }
    }

    /// Resolves a type key, name, or id into a type id.
    ///
    /// Same matching rules as [`resolve_type`](AnytypeClient::resolve_type);
    /// an id is returned unchanged without a server round trip.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if nothing matches
    /// - [`AnytypeError::Ambiguous`] if more than one type matches
    pub async fn resolve_type_id(
        &self,
        space_id: &str,
        key_or_id: impl Into<String>,
    ) -> Result<String> {
        let key_or_id = key_or_id.into();
        if let Some(stripped) = key_or_id.strip_prefix('@') {
            return match self
                .scan_type_matches(space_id, stripped, TypeMatchMode::Key)
                .await?
            {
                MatchClassification::None => Err(not_found("type", stripped)),
                MatchClassification::Unique(typ) => Ok(typ.id),
                MatchClassification::Ambiguous(candidates) => {
                    Err(ambiguous("type", stripped, candidates))
                }
            };
        }
        if looks_like_object_id(&key_or_id) {
            return Ok(key_or_id);
        }
        if starts_with_uppercase(&key_or_id) {
            return Ok(self.resolve_type_by_name(space_id, &key_or_id).await?.id);
        }
        match self
            .scan_type_matches(space_id, &key_or_id, TypeMatchMode::Any)
            .await?
        {
            MatchClassification::None => Err(not_found("type", &key_or_id)),
            MatchClassification::Unique(typ) => Ok(typ.id),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("type", &key_or_id, candidates))
            }
        }
    }

    /// Resolves an array of type keys, names, or ids into type ids.
    ///
    /// Batch form of [`resolve_type_id`](AnytypeClient::resolve_type_id);
    /// fails on the first item that does not resolve.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if an item matches nothing
    /// - [`AnytypeError::Ambiguous`] if an item matches more than one type
    pub async fn resolve_type_ids(&self, space_id: &str, types: &[String]) -> Result<Vec<String>> {
        let mut resolved = Vec::with_capacity(types.len());
        for type_key in types {
            resolved.push(self.resolve_type_id(space_id, type_key).await?);
        }
        Ok(resolved)
    }

    /// Resolves a type key, name, or id into a type key.
    ///
    /// Same matching rules as [`resolve_type`](AnytypeClient::resolve_type);
    /// a `@key` value is unwrapped without a server round trip. An explicit
    /// type id performs the same single cache-independent, identity-checked
    /// GET as `resolve_type`.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if nothing matches
    /// - [`AnytypeError::Ambiguous`] if more than one type matches
    pub async fn resolve_type_key(
        &self,
        space_id: &str,
        key_or_name: impl Into<String>,
    ) -> Result<String> {
        let key_or_name = key_or_name.into();
        if let Some(stripped) = key_or_name.strip_prefix('@') {
            return Ok(stripped.to_string());
        }
        if looks_like_object_id(&key_or_name) {
            let typ = self.get_type(space_id, &key_or_name).get_direct().await?;
            return Ok(typ.key);
        }
        if starts_with_uppercase(&key_or_name) {
            return Ok(self.resolve_type_by_name(space_id, &key_or_name).await?.key);
        }
        match self
            .scan_type_matches(space_id, &key_or_name, TypeMatchMode::Any)
            .await?
        {
            MatchClassification::None => Err(not_found("type", &key_or_name)),
            MatchClassification::Unique(typ) => Ok(typ.key),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("type", &key_or_name, candidates))
            }
        }
    }

    /// Resolves a template id or unique name into a fully verified template.
    ///
    /// CID-shaped ids use a direct GET. Other references are scanned with an
    /// exact 1,000-row budget; an exact id wins even when earlier rows made the
    /// name ambiguous, exact-case names win over case-insensitive names, and
    /// duplicate rows with one stable id remain one candidate. The selected
    /// template is fetched by id and must be non-archived, belong to the
    /// supplied space, and expose the canonical safe non-archived `template`
    /// type. The owning type is established by the already validated endpoint
    /// path; Anytype returns the generic template type on template objects.
    ///
    /// # Errors
    ///
    /// Returns [`AnytypeError::NotFound`] or [`AnytypeError::Ambiguous`] for a
    /// complete scan, [`AnytypeError::ResolutionLimitExceeded`] when uniqueness
    /// cannot be proven inside the scan budget, and a fixed upstream error for
    /// malformed pagination or mismatched final identity.
    pub async fn resolve_template(
        &self,
        space_id: &str,
        type_id: &str,
        template_id_or_name: &str,
    ) -> Result<Object> {
        if looks_like_object_id(template_id_or_name) {
            let template = self
                .template(space_id, type_id, template_id_or_name)
                .get()
                .await?;
            validate_resolved_template(&template, space_id, template_id_or_name, None)?;
            return Ok(template);
        }

        let mut matches = TemplateMatchAccumulator::new(template_id_or_name, space_id);
        let mut scanned = 0usize;
        let mut offset = 0u32;

        loop {
            let remaining = MAX_RESOLVE_SCAN_ITEMS.saturating_sub(scanned);
            if remaining == 0 {
                return Err(resolution_limit("template", template_id_or_name));
            }
            let requested_limit = RESOLVE_PAGE_SIZE.min(remaining as u32);
            let page = self
                .templates(space_id, type_id)
                .limit(requested_limit)
                .offset(offset)
                .list()
                .await?;
            let returned = page.items.len();
            let page_limit = page.pagination.limit;
            if page.pagination.offset != offset
                || page_limit == 0
                || page_limit > requested_limit
                || returned > page_limit as usize
                || (page.pagination.has_more && returned == 0)
            {
                return Err(malformed_template_resolution());
            }

            for template in page.items.iter().cloned() {
                scanned += 1;
                if matches.push(template) {
                    break;
                }
            }
            if matches.has_exact_id() {
                break;
            }
            if !page.pagination.has_more {
                break;
            }
            if scanned == MAX_RESOLVE_SCAN_ITEMS {
                return Err(resolution_limit("template", template_id_or_name));
            }
            offset = offset
                .checked_add(page_limit)
                .filter(|next| *next > offset)
                .ok_or_else(malformed_template_resolution)?;
        }

        let selected = match matches.finish()? {
            MatchClassification::None => {
                return Err(not_found("template", template_id_or_name));
            }
            MatchClassification::Unique(template) => template,
            MatchClassification::Ambiguous(candidates) => {
                return Err(ambiguous("template", template_id_or_name, candidates));
            }
        };
        let selected_id = selected.id.clone();
        let template = self.template(space_id, type_id, &selected_id).get().await?;
        validate_resolved_template(&template, space_id, &selected_id, Some(&selected))?;
        Ok(template)
    }

    /// Resolves a view name or id into a view id, for a list (collection
    /// or query).
    ///
    /// An exact (case-sensitive) name match wins; otherwise the name is
    /// matched case-insensitively.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if no view has that name
    /// - [`AnytypeError::Ambiguous`] if more than one view has that name
    pub async fn resolve_view_id(
        &self,
        space_id: &str,
        list_id: &str,
        view_id_or_name: &str,
    ) -> Result<String> {
        let mut views = self
            .list_views(space_id, list_id)
            .limit(RESOLVE_PAGE_SIZE)
            .list()
            .await?
            .into_stream();
        let mut matches = ViewMatchAccumulator::new(view_id_or_name);
        let mut scanned = 0;
        while let Some(view) = views.next().await {
            let view = view?;
            if scanned == MAX_RESOLVE_SCAN_ITEMS {
                return Err(resolution_limit("view", view_id_or_name));
            }
            scanned += 1;
            if let Some(id) = matches.push(view) {
                return Ok(id);
            }
        }

        match matches.finish() {
            MatchClassification::None => Err(not_found("view", view_id_or_name)),
            MatchClassification::Unique(view) => Ok(view.id),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("view", view_id_or_name, candidates))
            }
        }
    }

    /// Resolves a property key or id into a property id.
    ///
    /// An id is returned unchanged without a server round trip.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if no property has that key
    pub async fn resolve_property_id(
        &self,
        space_id: &str,
        key_or_id: impl Into<String>,
    ) -> Result<String> {
        let key_or_id = key_or_id.into();
        if looks_like_object_id(&key_or_id) {
            return Ok(key_or_id);
        }
        let needle = key_or_id.trim().to_lowercase();
        match scan_paged_matches(
            self.properties(space_id)
                .limit(RESOLVE_PAGE_SIZE)
                .list()
                .await?,
            "property",
            &key_or_id,
            |property| property.key == needle,
            property_candidate,
        )
        .await?
        {
            MatchClassification::None => Err(not_found("property", &key_or_id)),
            MatchClassification::Unique(property) => Ok(property.id),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("property", &key_or_id, candidates))
            }
        }
    }

    /// Resolves a chat reference into a [`ChatTarget`].
    ///
    /// Name resolution and default-space-chat discovery require a gRPC
    /// backend. An exact chat ID with an explicit space needs no gRPC call.
    ///
    /// The reference forms, in order of interpretation:
    ///
    /// - space given: `chat_id_or_name` is a chat id, or a chat name
    ///   resolved within that space
    /// - no space, and `chat_id_or_name` is an id: a chat id if such a chat
    ///   exists, else a space id whose default chat
    ///   ([`DEFAULT_CHAT_NAME`]) is resolved
    /// - no space, and `chat_id_or_name` is a name: a space name whose
    ///   default chat is resolved
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if a chat name matches nothing in the space
    /// - [`AnytypeError::Ambiguous`] if a chat or space name is not unique
    /// - [`AnytypeError::Validation`] if a bare chat name is given without
    ///   any space context to resolve it in
    pub async fn resolve_chat_target(
        &self,
        space_id: Option<&str>,
        chat_id_or_name: &str,
    ) -> Result<ChatTarget> {
        if let Some(space_id) = space_id {
            let chat_id = if looks_like_object_id(chat_id_or_name) {
                chat_id_or_name.to_string()
            } else {
                self.resolve_chat_id_in_space(space_id, chat_id_or_name)
                    .await?
            };
            return Ok(ChatTarget {
                space_id: Some(space_id.to_string()),
                chat_id,
            });
        }

        if looks_like_object_id(chat_id_or_name) {
            if self.chat_exists(chat_id_or_name).await? {
                return Ok(ChatTarget {
                    space_id: None,
                    chat_id: chat_id_or_name.to_string(),
                });
            }
            if let Some(space_id) = self.find_space_id_by_id(chat_id_or_name).await? {
                let chat_id = self
                    .resolve_chat_id_in_space(&space_id, DEFAULT_CHAT_NAME)
                    .await?;
                return Ok(ChatTarget {
                    space_id: Some(space_id),
                    chat_id,
                });
            }
            return Ok(ChatTarget {
                space_id: None,
                chat_id: chat_id_or_name.to_string(),
            });
        }

        if let Some(space_id) = self.find_space_id_by_name(chat_id_or_name).await? {
            let chat_id = self
                .resolve_chat_id_in_space(&space_id, DEFAULT_CHAT_NAME)
                .await?;
            return Ok(ChatTarget {
                space_id: Some(space_id),
                chat_id,
            });
        }

        Err(AnytypeError::Validation {
            message: format!(
                "chat name requires a space context (space id/name) to resolve: {chat_id_or_name}"
            ),
        })
    }

    /// Resolves an array of chat references into chat ids.
    ///
    /// Inputs that are not exact chat IDs may require a gRPC backend, as
    /// described by [`resolve_chat_target`](AnytypeClient::resolve_chat_target).
    ///
    /// Batch form of
    /// [`resolve_chat_target`](AnytypeClient::resolve_chat_target);
    /// fails on the first item that does not resolve.
    ///
    /// # Errors
    /// - any error returned by [`resolve_chat_target`](AnytypeClient::resolve_chat_target)
    pub async fn resolve_chat_ids(
        &self,
        space_id: Option<&str>,
        chats: &[String],
    ) -> Result<Vec<String>> {
        let mut resolved = Vec::with_capacity(chats.len());
        for chat in chats {
            let target = self.resolve_chat_target(space_id, chat).await?;
            resolved.push(target.chat_id);
        }
        Ok(resolved)
    }

    /// Resolves a chat id into its display name, falling back to the id
    /// when the chat has no name.
    ///
    /// This method requires a gRPC backend.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if no space is given and no chat has that id
    pub async fn resolve_chat_name(&self, space_id: Option<&str>, chat_id: &str) -> Result<String> {
        if let Some(space_id) = space_id {
            let chat = self.chats().get_chat(space_id, chat_id).get().await?;
            return Ok(chat.name.unwrap_or_else(|| chat_id.to_string()));
        }

        let chats = self.chats().list_chats().list().await?;
        let chat = chats
            .items
            .into_iter()
            .find(|chat| chat.id == chat_id)
            .ok_or_else(|| not_found("chat", chat_id))?;
        Ok(chat.name.unwrap_or_else(|| chat_id.to_string()))
    }

    /// Resolves a message reference into its message id.
    ///
    /// Resolving an order ID requires a gRPC backend. A value that already
    /// looks like a message ID is returned without a backend call.
    ///
    /// Accepts either a message id (returned unchanged when it already looks
    /// like an object id ([`looks_like_object_id`])) or a chat `order_id`. An
    /// order id is looked up in `chat_id` with a single bounded page that
    /// includes the boundary, and the matching message's id is returned.
    ///
    /// Order ids are the opaque values carried by
    /// [`ChatMessage::order_id`](crate::chats::ChatMessage); pass them exactly
    /// as the server reports them. Any textual encoding a caller applies to
    /// carry an order id through another medium (for example hex-encoding it
    /// for a command line) must be undone before calling this method.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if no message in `chat_id` has that order id
    /// - any error returned while listing chat messages
    pub async fn resolve_message_id(
        &self,
        chat_id: &str,
        message_id_or_order_id: &str,
    ) -> Result<String> {
        if looks_like_object_id(message_id_or_order_id) {
            return Ok(message_id_or_order_id.to_string());
        }

        let page = self
            .chats()
            .list_messages(chat_id)
            .after(message_id_or_order_id)
            .before(message_id_or_order_id)
            .include_boundary(true)
            .limit(1)
            .list_page()
            .await?;

        page.messages
            .into_iter()
            .find(|message| message.order_id == message_id_or_order_id)
            .map(|message| message.id)
            .ok_or_else(|| not_found("message", message_id_or_order_id))
    }

    /// Resolves an array of message references into message ids.
    ///
    /// Any order-ID input requires a gRPC backend; exact message IDs do not.
    ///
    /// Batch form of
    /// [`resolve_message_id`](AnytypeClient::resolve_message_id); fails on the
    /// first item that does not resolve.
    ///
    /// # Errors
    /// - any error returned by [`resolve_message_id`](AnytypeClient::resolve_message_id)
    pub async fn resolve_message_ids(
        &self,
        chat_id: &str,
        message_ids: &[String],
    ) -> Result<Vec<String>> {
        let mut resolved = Vec::with_capacity(message_ids.len());
        for message_id in message_ids {
            resolved.push(self.resolve_message_id(chat_id, message_id).await?);
        }
        Ok(resolved)
    }

    /// Resolves a chat name (case-insensitive) into a chat id within a space.
    async fn resolve_chat_id_in_space(
        &self,
        space_id: &str,
        chat_id_or_name: &str,
    ) -> Result<String> {
        let needle = chat_id_or_name.to_lowercase();
        let mut accumulator = MatchAccumulator::new();
        let mut scanned = 0;
        let mut offset = 0;
        loop {
            let result = self
                .chats()
                .search_chats_in(space_id)
                .text(chat_id_or_name)
                .limit(RESOLVE_PAGE_SIZE)
                .offset(offset)
                .search()
                .await?;
            let page_len = result.items.len();
            for chat in result.items {
                if scanned == MAX_RESOLVE_SCAN_ITEMS {
                    return Err(resolution_limit("chat", chat_id_or_name));
                }
                scanned += 1;
                if chat.name.as_deref().unwrap_or("").to_lowercase() == needle {
                    let candidate = object_candidate(&chat);
                    accumulator.push(chat, candidate);
                }
            }
            if page_len < RESOLVE_PAGE_SIZE as usize {
                break;
            }
            offset = offset
                .checked_add(RESOLVE_PAGE_SIZE)
                .ok_or_else(|| resolution_limit("chat", chat_id_or_name))?;
        }

        match accumulator.finish() {
            MatchClassification::None => Err(not_found("chat", chat_id_or_name)),
            MatchClassification::Unique(chat) => Ok(chat.id),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("chat", chat_id_or_name, candidates))
            }
        }
    }

    /// Returns the space id if a space with this id is accessible.
    async fn find_space_id_by_id(&self, space_id: &str) -> Result<Option<String>> {
        let mut spaces = self
            .spaces()
            .limit(RESOLVE_PAGE_SIZE)
            .list()
            .await?
            .into_stream();
        let mut scanned = 0;
        while let Some(space) = spaces.next().await {
            let space = space?;
            if scanned == MAX_RESOLVE_SCAN_ITEMS {
                return Err(resolution_limit("space", space_id));
            }
            scanned += 1;
            if space.id == space_id {
                return Ok(Some(space.id));
            }
        }
        Ok(None)
    }

    /// Returns true if a chat with this id is accessible.
    async fn chat_exists(&self, chat_id: &str) -> Result<bool> {
        let mut spaces = self
            .spaces()
            .limit(RESOLVE_PAGE_SIZE)
            .list()
            .await?
            .into_stream();
        let mut budget = ResolutionScanBudget::new();
        while let Some(space) = spaces.next().await {
            let space = space?;
            budget.record("chat", chat_id)?;
            let mut offset = 0;
            loop {
                let result = self
                    .chats()
                    .search_chats_in(&space.id)
                    .limit(RESOLVE_PAGE_SIZE)
                    .offset(offset)
                    .search()
                    .await?;
                let page_len = result.items.len();
                for chat in result.items {
                    budget.record("chat", chat_id)?;
                    if chat.id == chat_id {
                        return Ok(true);
                    }
                }
                if page_len < RESOLVE_PAGE_SIZE as usize {
                    break;
                }
                offset = offset
                    .checked_add(RESOLVE_PAGE_SIZE)
                    .ok_or_else(|| resolution_limit("chat", chat_id))?;
            }
        }
        Ok(false)
    }

    /// Finds a space id by case-insensitive name match; `Ok(None)` when
    /// no space matches.
    async fn find_space_id_by_name(&self, space_name: &str) -> Result<Option<String>> {
        let needle = space_name.to_lowercase();
        match scan_paged_matches(
            self.spaces().limit(RESOLVE_PAGE_SIZE).list().await?,
            "space",
            space_name,
            |space| space.name.to_lowercase() == needle,
            |space| ResolveCandidate::new(&space.id, &space.name),
        )
        .await?
        {
            MatchClassification::None => Ok(None),
            MatchClassification::Unique(space) => Ok(Some(space.id)),
            MatchClassification::Ambiguous(candidates) => {
                Err(ambiguous("space", space_name, candidates))
            }
        }
    }

    /// Resolves a type name (case-insensitive) into the full [`Type`].
    async fn resolve_type_by_name(&self, space_id: &str, name: &str) -> Result<Type> {
        match self
            .scan_type_matches(space_id, name, TypeMatchMode::Name)
            .await?
        {
            MatchClassification::None => Err(not_found("type", name)),
            MatchClassification::Unique(typ) => Ok(typ),
            MatchClassification::Ambiguous(candidates) => Err(ambiguous("type", name, candidates)),
        }
    }

    async fn scan_type_matches(
        &self,
        space_id: &str,
        text: &str,
        mode: TypeMatchMode,
    ) -> Result<MatchClassification<Type>> {
        let needle = text.trim().to_lowercase();
        scan_paged_matches(
            self.types(space_id).limit(RESOLVE_PAGE_SIZE).list().await?,
            "type",
            text,
            |typ| {
                !typ.archived
                    && match mode {
                        TypeMatchMode::Key => typ.key == needle,
                        TypeMatchMode::Name => {
                            typ.name.as_deref().unwrap_or("").to_lowercase() == needle
                        }
                        TypeMatchMode::Any => {
                            typ.id == needle
                                || typ.key == needle
                                || typ.name.as_deref().unwrap_or("").to_lowercase() == needle
                                || typ.plural_name.as_deref().unwrap_or("").to_lowercase() == needle
                        }
                    }
            },
            type_candidate,
        )
        .await
    }
}

#[derive(Clone, Copy)]
enum TypeMatchMode {
    Any,
    Key,
    Name,
}

fn starts_with_uppercase(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

fn not_found(obj_type: &str, key: &str) -> AnytypeError {
    AnytypeError::NotFound {
        obj_type: obj_type.to_string(),
        key: key.to_string(),
    }
}

fn ambiguous(
    obj_type: &str,
    key: &str,
    candidates: impl IntoIterator<Item = ResolveCandidate>,
) -> AnytypeError {
    AnytypeError::Ambiguous {
        obj_type: obj_type.to_string(),
        key: key.to_string(),
        candidates: candidates
            .into_iter()
            .take(MAX_RESOLVE_CANDIDATES)
            .collect(),
    }
}

enum MatchClassification<T> {
    None,
    Unique(T),
    Ambiguous(Vec<ResolveCandidate>),
}

struct ResolutionScanBudget {
    scanned: usize,
}

impl ResolutionScanBudget {
    const fn new() -> Self {
        Self { scanned: 0 }
    }

    fn record(&mut self, obj_type: &str, key: &str) -> Result<()> {
        if self.scanned == MAX_RESOLVE_SCAN_ITEMS {
            return Err(resolution_limit(obj_type, key));
        }
        self.scanned += 1;
        Ok(())
    }
}

struct MatchAccumulator<T> {
    unique: Option<(ResolveCandidate, T)>,
    ambiguous: bool,
    candidates: Vec<ResolveCandidate>,
}

struct ViewMatchAccumulator {
    target: String,
    needle: String,
    exact: MatchAccumulator<crate::views::View>,
    case_insensitive: MatchAccumulator<crate::views::View>,
}

struct TemplateMatchAccumulator {
    target: String,
    space_id: String,
    exact_id: Option<Object>,
    exact_names: TemplateNameAccumulator,
    case_insensitive_names: TemplateNameAccumulator,
}

impl TemplateMatchAccumulator {
    fn new(target: &str, space_id: &str) -> Self {
        Self {
            target: target.to_string(),
            space_id: space_id.to_string(),
            exact_id: None,
            exact_names: TemplateNameAccumulator::new(),
            case_insensitive_names: TemplateNameAccumulator::new(),
        }
    }

    fn push(&mut self, template: Object) -> bool {
        if template.archived {
            return false;
        }
        if template.id == self.target {
            self.exact_id = Some(template);
            return true;
        }
        let Some(name) = template.name.as_deref() else {
            return false;
        };
        let candidate = object_candidate(&template);
        let safe = candidate_is_safe(&candidate)
            && validate_template_identity(&template, &self.space_id, &template.id).is_ok();
        if name == self.target {
            self.exact_names.push(template, candidate, safe);
        } else if name.eq_ignore_ascii_case(&self.target) {
            self.case_insensitive_names.push(template, candidate, safe);
        }
        false
    }

    const fn has_exact_id(&self) -> bool {
        self.exact_id.is_some()
    }

    fn finish(self) -> Result<MatchClassification<Object>> {
        if let Some(exact_id) = self.exact_id {
            return Ok(MatchClassification::Unique(exact_id));
        }
        match self.exact_names.finish()? {
            MatchClassification::None => self.case_insensitive_names.finish(),
            exact => Ok(exact),
        }
    }
}

struct TemplateNameAccumulator {
    safe_matches: MatchAccumulator<Object>,
    safe_ids: HashSet<String>,
    unsafe_ids: HashSet<String>,
    has_unrepresentable_identity: bool,
}

impl TemplateNameAccumulator {
    fn new() -> Self {
        Self {
            safe_matches: MatchAccumulator::new(),
            safe_ids: HashSet::new(),
            unsafe_ids: HashSet::new(),
            has_unrepresentable_identity: false,
        }
    }

    fn push(&mut self, template: Object, candidate: ResolveCandidate, safe: bool) {
        if safe {
            self.unsafe_ids.remove(candidate.id());
            self.safe_ids.insert(candidate.id().to_owned());
            self.safe_matches.push(template, candidate);
            return;
        }

        let id = candidate.id();
        if self.safe_ids.contains(id) {
            return;
        }
        if stable_template_identity_is_bounded(id) {
            self.unsafe_ids.insert(id.to_owned());
        } else {
            self.has_unrepresentable_identity = true;
        }
    }

    fn finish(self) -> Result<MatchClassification<Object>> {
        if self.has_unrepresentable_identity || !self.unsafe_ids.is_empty() {
            return Err(malformed_template_resolution());
        }
        Ok(self.safe_matches.finish())
    }
}

fn stable_template_identity_is_bounded(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_RESOLVE_CANDIDATE_ID_CHARS
}

impl ViewMatchAccumulator {
    fn new(target: &str) -> Self {
        Self {
            target: target.to_string(),
            needle: target.to_lowercase(),
            exact: MatchAccumulator::new(),
            case_insensitive: MatchAccumulator::new(),
        }
    }

    fn push(&mut self, view: crate::views::View) -> Option<String> {
        if view.id == self.target {
            return Some(view.id);
        }
        if view.name.as_deref() == Some(self.target.as_str()) {
            let candidate = view_candidate(&view);
            self.exact.push(view, candidate);
        } else if view.name.as_deref().unwrap_or("").to_lowercase() == self.needle {
            let candidate = view_candidate(&view);
            self.case_insensitive.push(view, candidate);
        }
        None
    }

    fn finish(self) -> MatchClassification<crate::views::View> {
        match self.exact.finish() {
            MatchClassification::None => self.case_insensitive.finish(),
            exact => exact,
        }
    }
}

impl<T> MatchAccumulator<T> {
    fn new() -> Self {
        Self {
            unique: None,
            ambiguous: false,
            candidates: Vec::new(),
        }
    }

    fn push(&mut self, item: T, candidate: ResolveCandidate) {
        if !self.ambiguous {
            match &mut self.unique {
                None => {
                    self.unique = Some((candidate, item));
                }
                Some((current, current_item)) if current.id == candidate.id => {
                    if compare_duplicate_representatives(&candidate, current).is_lt() {
                        *current = candidate;
                        *current_item = item;
                    }
                }
                Some(_) => {
                    self.ambiguous = true;
                    let (current, _) = self.unique.take().expect("unique match exists");
                    insert_bounded_candidate(&mut self.candidates, current);
                    insert_bounded_candidate(&mut self.candidates, candidate);
                }
            }
            return;
        }

        insert_bounded_candidate(&mut self.candidates, candidate);
    }

    fn finish(self) -> MatchClassification<T> {
        if self.ambiguous {
            MatchClassification::Ambiguous(self.candidates)
        } else if let Some((_, item)) = self.unique {
            MatchClassification::Unique(item)
        } else {
            MatchClassification::None
        }
    }
}

#[cfg(test)]
fn classify_matches<T>(
    matches: impl IntoIterator<Item = T>,
    candidate_for: impl Fn(&T) -> ResolveCandidate,
) -> std::result::Result<MatchClassification<T>, ResolutionScanLimit> {
    let mut accumulator = MatchAccumulator::new();
    for (index, item) in matches.into_iter().enumerate() {
        if index == MAX_RESOLVE_SCAN_ITEMS {
            return Err(ResolutionScanLimit);
        }
        let candidate = candidate_for(&item);
        accumulator.push(item, candidate);
    }
    Ok(accumulator.finish())
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct ResolutionScanLimit;

fn insert_bounded_candidate(candidates: &mut Vec<ResolveCandidate>, candidate: ResolveCandidate) {
    if !candidate_is_safe(&candidate) {
        return;
    }
    if let Some(existing) = candidates
        .iter_mut()
        .find(|existing| existing.id == candidate.id)
    {
        if compare_candidates(&candidate, existing).is_lt() {
            *existing = candidate;
            candidates.sort_by(compare_candidates);
        }
        return;
    }

    if candidates.len() < MAX_RESOLVE_CANDIDATES {
        candidates.push(candidate);
        candidates.sort_by(compare_candidates);
    } else if candidates
        .last()
        .is_some_and(|largest| compare_candidates(&candidate, largest).is_lt())
    {
        candidates.pop();
        candidates.push(candidate);
        candidates.sort_by(compare_candidates);
    }
}

fn candidate_is_safe(candidate: &ResolveCandidate) -> bool {
    let id = candidate.id();
    !id.is_empty()
        && !matches!(id, "." | "..")
        && id.chars().count() <= MAX_RESOLVE_CANDIDATE_ID_CHARS
        && id
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || b"._~-".contains(&character))
        && candidate.name().chars().count() <= MAX_RESOLVE_CANDIDATE_NAME_CHARS
}

fn compare_candidates(left: &ResolveCandidate, right: &ResolveCandidate) -> std::cmp::Ordering {
    left.name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then_with(|| left.name.cmp(&right.name))
        .then_with(|| left.id.cmp(&right.id))
}

fn compare_duplicate_representatives(
    left: &ResolveCandidate,
    right: &ResolveCandidate,
) -> std::cmp::Ordering {
    match (candidate_is_safe(left), candidate_is_safe(right)) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (true, true) | (false, false) => compare_candidates(left, right),
    }
}

fn type_candidate(typ: &Type) -> ResolveCandidate {
    ResolveCandidate::new(&typ.id, typ.name.as_deref().unwrap_or(&typ.key))
}

fn view_candidate(view: &crate::views::View) -> ResolveCandidate {
    ResolveCandidate::new(&view.id, view.name.as_deref().unwrap_or(&view.id))
}

fn object_candidate(object: &crate::objects::Object) -> ResolveCandidate {
    ResolveCandidate::new(&object.id, object.name.as_deref().unwrap_or(&object.id))
}

fn validate_resolved_template(
    template: &Object,
    space_id: &str,
    selected_id: &str,
    listed: Option<&Object>,
) -> Result<()> {
    let template_type = validate_template_identity(template, space_id, selected_id)?;
    if let Some(listed) = listed {
        let listed_type = validate_template_identity(listed, space_id, selected_id)?;
        if listed_type.id != template_type.id || listed_type.key != template_type.key {
            return Err(malformed_template_resolution());
        }
    }
    Ok(())
}

fn validate_template_identity<'a>(
    template: &'a Object,
    space_id: &str,
    selected_id: &str,
) -> Result<&'a Type> {
    let Some(template_type) = template.r#type.as_ref() else {
        return Err(malformed_template_resolution());
    };
    if template.archived
        || template.id != selected_id
        || template.space_id != space_id
        || template_type.archived
        || template_type.key != "template"
        || !looks_like_object_id(&template_type.id)
    {
        return Err(malformed_template_resolution());
    }
    Ok(template_type)
}

fn malformed_template_resolution() -> AnytypeError {
    AnytypeError::Other {
        message: "template resolver received malformed upstream state".to_owned(),
    }
}

fn property_candidate(property: &crate::properties::Property) -> ResolveCandidate {
    ResolveCandidate::new(&property.id, &property.name)
}

pub(crate) fn resolution_limit(obj_type: &str, key: &str) -> AnytypeError {
    AnytypeError::ResolutionLimitExceeded {
        obj_type: obj_type.to_string(),
        key: key.to_string(),
        limit: MAX_RESOLVE_SCAN_ITEMS,
    }
}

async fn scan_paged_matches<T>(
    page: crate::paged::PagedResult<T>,
    obj_type: &str,
    key: &str,
    matches: impl Fn(&T) -> bool,
    candidate_for: impl Fn(&T) -> ResolveCandidate,
) -> Result<MatchClassification<T>>
where
    T: serde::de::DeserializeOwned + Send + 'static,
{
    let mut stream = page.into_stream();
    let mut accumulator = MatchAccumulator::new();
    let mut scanned = 0;
    while let Some(item) = stream.next().await {
        let item = item?;
        if scanned == MAX_RESOLVE_SCAN_ITEMS {
            return Err(resolution_limit(obj_type, key));
        }
        scanned += 1;
        if matches(&item) {
            let candidate = candidate_for(&item);
            accumulator.push(item, candidate);
        }
    }
    Ok(accumulator.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn empty_page_server() -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture server");
        let address = listener.local_addr().expect("fixture address");
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept fixture request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let body =
                r#"{"items":[],"pagination":{"has_more":false,"limit":99,"offset":0,"total":0}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            String::from_utf8(request).expect("request is utf-8")
        });
        (format!("http://{address}"), task)
    }

    async fn paged_fixture_server(
        bodies: Vec<String>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind paged fixture server");
        let address = listener.local_addr().expect("paged fixture address");
        let task = tokio::spawn(async move {
            let mut requests = Vec::with_capacity(bodies.len());
            for body in bodies {
                let (mut stream, _) = listener.accept().await.expect("accept paged request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.expect("read paged request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write paged response");
                requests.push(String::from_utf8(request).expect("request is utf-8"));
            }
            requests
        });
        (format!("http://{address}"), task)
    }

    #[derive(Debug, Default)]
    struct TypeRouteTraffic {
        requests: Vec<String>,
        type_list_pages: usize,
        direct_type_gets: usize,
    }

    async fn route_aware_type_server() -> (
        String,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<TypeRouteTraffic>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind route-aware type fixture server");
        let address = listener.local_addr().expect("type fixture address");
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut traffic = TypeRouteTraffic::default();
            loop {
                let accepted = tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => accepted,
                };
                let (mut stream, _) = accepted.expect("accept route-aware type request");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let read = stream.read(&mut buffer).await.expect("read type request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).expect("request is utf-8");
                let request_line = request.lines().next().expect("type request line");
                let path = request_line
                    .split_ascii_whitespace()
                    .nth(1)
                    .expect("type request path");
                let collection_path = format!("/v1/spaces/{SPACE_ID}/types");
                let direct_path = format!("{collection_path}/{OBJECT_ID}");
                let body = if path == collection_path
                    || path.starts_with(&format!("{collection_path}?"))
                {
                    let page = traffic.type_list_pages;
                    traffic.type_list_pages += 1;
                    if page == 0 {
                        serde_json::json!({
                            "items": [{
                                "archived": false,
                                "id": OTHER_OBJECT_ID,
                                "key": "note",
                                "name": "Note"
                            }],
                            "pagination": {
                                "has_more": true,
                                "limit": 100,
                                "offset": 0,
                                "total": 101
                            }
                        })
                        .to_string()
                    } else {
                        serde_json::json!({
                            "items": [{
                                "archived": false,
                                "id": OBJECT_ID,
                                "key": "page",
                                "name": "Page"
                            }],
                            "pagination": {
                                "has_more": false,
                                "limit": 100,
                                "offset": 100,
                                "total": 101
                            }
                        })
                        .to_string()
                    }
                } else if path == direct_path {
                    traffic.direct_type_gets += 1;
                    serde_json::json!({
                        "type": {
                            "archived": false,
                            "id": OBJECT_ID,
                            "key": "page",
                            "name": "Page"
                        }
                    })
                    .to_string()
                } else {
                    panic!("unexpected type fixture route: {request_line}");
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                traffic.requests.push(request);
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write type response");
            }
            traffic
        });
        (format!("http://{address}"), shutdown_tx, task)
    }

    fn fixture_client(base_url: String) -> AnytypeClient {
        let mut config = crate::client::ClientConfig::default().app_name("resolve-http-fixture");
        config.base_url = Some(base_url);
        config.keystore = Some("env".to_string());
        let client = AnytypeClient::with_config(config).expect("fixture client");
        client.set_api_key(crate::keystore::HttpCredentials::new("fixture-token"));
        client
    }

    fn template_value(
        id: &str,
        name: Option<&str>,
        archived: bool,
        space_id: &str,
        type_id: &str,
        type_key: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "archived": archived,
            "id": id,
            "name": name,
            "space_id": space_id,
            "type": {
                "archived": false,
                "id": type_id,
                "key": type_key
            }
        })
    }

    fn template_page(
        items: Vec<serde_json::Value>,
        has_more: bool,
        limit: u32,
        offset: u32,
    ) -> String {
        serde_json::json!({
            "items": items,
            "pagination": {
                "has_more": has_more,
                "limit": limit,
                "offset": offset,
                "total": 0
            }
        })
        .to_string()
    }

    // a valid space id (CID.HASH form) that passes looks_like_object_id
    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    // a valid bare CID object id (59 chars)
    const OBJECT_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";
    const OTHER_OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const TEMPLATE_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
    const OTHER_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z";

    #[test]
    fn template_match_later_exact_id_beats_earlier_ambiguous_names() {
        let object = |id: &str, name: &str| {
            serde_json::from_value::<Object>(template_value(
                id,
                Some(name),
                false,
                SPACE_ID,
                OTHER_ID,
                "template",
            ))
            .expect("valid template object")
        };
        let mut matches = TemplateMatchAccumulator::new(OBJECT_ID, SPACE_ID);
        assert!(!matches.push(object(TEMPLATE_ID, OBJECT_ID)));
        assert!(!matches.push(object(OTHER_ID, OBJECT_ID)));
        assert!(matches.push(object(OBJECT_ID, "Different")));

        let MatchClassification::Unique(selected) = matches.finish().expect("safe matches") else {
            panic!("later exact id must override earlier ambiguous names");
        };
        assert_eq!(selected.id, OBJECT_ID);
    }

    #[tokio::test]
    async fn template_direct_id_uses_one_get_and_revalidates_identity() {
        let body = serde_json::json!({
            "template": template_value(TEMPLATE_ID, Some("Starter"), false, SPACE_ID, OTHER_ID, "template")
        })
        .to_string();
        let (base_url, requests) = paged_fixture_server(vec![body]).await;

        let resolved = fixture_client(base_url)
            .resolve_template(SPACE_ID, OBJECT_ID, TEMPLATE_ID)
            .await
            .expect("direct template id");
        assert_eq!(resolved.id, TEMPLATE_ID);
        let requests = requests.await.expect("direct template fixture");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with(&format!(
            "GET /v1/spaces/{SPACE_ID}/types/{OBJECT_ID}/templates/{TEMPLATE_ID} HTTP/1.1"
        )));
    }

    #[tokio::test]
    async fn template_scan_deduplicates_and_ignores_unsafe_unrelated_rows() {
        let duplicate = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        let unsafe_row = template_value(
            "../unsafe",
            Some("Other"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        let first = template_page(
            vec![duplicate.clone(), duplicate, unsafe_row],
            true,
            RESOLVE_PAGE_SIZE,
            0,
        );
        let unrelated = template_value(
            OTHER_ID,
            Some("Different"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        let second = template_page(vec![unrelated], false, RESOLVE_PAGE_SIZE, RESOLVE_PAGE_SIZE);
        let final_get = serde_json::json!({
            "template": template_value(TEMPLATE_ID, Some("Starter"), false, SPACE_ID, OTHER_ID, "template")
        })
        .to_string();
        let (base_url, requests) = paged_fixture_server(vec![first, second, final_get]).await;

        let resolved = fixture_client(base_url)
            .resolve_template(SPACE_ID, OBJECT_ID, "Starter")
            .await
            .expect("later exact id");
        assert_eq!(resolved.id, TEMPLATE_ID);
        let requests = requests.await.expect("later id fixture");
        assert_eq!(requests.len(), 3);
        assert!(requests[2].contains(&format!("/templates/{TEMPLATE_ID} HTTP/1.1")));
    }

    #[tokio::test]
    async fn template_scan_fails_closed_for_distinct_unsafe_name_match_in_either_order() {
        let safe = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        let unsafe_match = template_value(
            "../SECRET_UNSAFE_ID",
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        let malformed_match = template_value(
            OTHER_ID,
            Some("Starter"),
            false,
            "../SECRET_BAD_SPACE",
            OTHER_ID,
            "template",
        );

        for conflicting in [unsafe_match, malformed_match] {
            for items in [
                vec![conflicting.clone(), safe.clone()],
                vec![safe.clone(), conflicting.clone()],
            ] {
                let (base_url, requests) =
                    paged_fixture_server(vec![template_page(items, false, RESOLVE_PAGE_SIZE, 0)])
                        .await;
                let error = fixture_client(base_url)
                    .resolve_template(SPACE_ID, OBJECT_ID, "Starter")
                    .await
                    .expect_err("malformed distinct match must prevent unique resolution");
                assert!(matches!(error, AnytypeError::Other { .. }));
                assert!(!format!("{error:?}").contains("SECRET"));
                assert_eq!(requests.await.expect("unsafe match fixture").len(), 1);
            }
        }
    }

    #[tokio::test]
    async fn template_scan_uses_safe_same_id_representative_in_either_order() {
        let safe = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        let malformed_duplicate = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            "../SECRET_BAD_SPACE",
            OTHER_ID,
            "template",
        );
        let final_get = serde_json::json!({"template": safe.clone()}).to_string();

        for items in [
            vec![malformed_duplicate.clone(), safe.clone()],
            vec![safe.clone(), malformed_duplicate.clone()],
        ] {
            let (base_url, requests) = paged_fixture_server(vec![
                template_page(items, false, RESOLVE_PAGE_SIZE, 0),
                final_get.clone(),
            ])
            .await;
            let resolved = fixture_client(base_url)
                .resolve_template(SPACE_ID, OBJECT_ID, "Starter")
                .await
                .expect("safe duplicate representative");
            assert_eq!(resolved.id, TEMPLATE_ID);
            let requests = requests.await.expect("safe duplicate fixture");
            assert_eq!(requests.len(), 2);
            assert!(requests[1].contains(&format!("/templates/{TEMPLATE_ID} HTTP/1.1")));
        }
    }

    #[tokio::test]
    async fn template_scan_is_exactly_bounded_and_excludes_archived_rows() {
        let mut bodies = Vec::new();
        for page_index in 0..10u32 {
            let offset = page_index * RESOLVE_PAGE_SIZE;
            let items = (0..RESOLVE_PAGE_SIZE)
                .map(|_| {
                    template_value(
                        OTHER_ID,
                        Some("Archived match"),
                        true,
                        SPACE_ID,
                        OTHER_ID,
                        "template",
                    )
                })
                .collect();
            bodies.push(template_page(items, true, RESOLVE_PAGE_SIZE, offset));
        }
        let selected = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        bodies.push(template_page(
            std::iter::once(selected.clone())
                .chain((1..10).map(|_| {
                    template_value(
                        OTHER_ID,
                        Some("Other"),
                        false,
                        SPACE_ID,
                        OTHER_ID,
                        "template",
                    )
                }))
                .collect(),
            false,
            10,
            990,
        ));
        bodies.push(serde_json::json!({"template": selected}).to_string());
        let (base_url, requests) = paged_fixture_server(bodies).await;

        let resolved = fixture_client(base_url)
            .resolve_template(SPACE_ID, OBJECT_ID, "Starter")
            .await
            .expect("match at exact scan boundary");
        assert_eq!(resolved.id, TEMPLATE_ID);
        let requests = requests.await.expect("bounded template fixture");
        assert_eq!(requests.len(), 12);
        assert!(requests[10].lines().next().unwrap().contains("limit=10"));
        assert!(requests[10].lines().next().unwrap().contains("offset=990"));
    }

    #[tokio::test]
    async fn template_scan_rejects_incomplete_or_mismatched_upstream_state() {
        let (base_url, requests) =
            paged_fixture_server(vec![template_page(Vec::new(), true, 10, 0)]).await;
        let error = fixture_client(base_url)
            .resolve_template(SPACE_ID, OBJECT_ID, "Missing")
            .await
            .expect_err("sparse scan cannot claim completion");
        assert!(matches!(error, AnytypeError::Other { .. }));
        assert_eq!(requests.await.expect("sparse fixture").len(), 1);

        let listed = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        let mismatched = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OBJECT_ID,
            "template",
        );
        let bodies = vec![
            template_page(vec![listed], false, RESOLVE_PAGE_SIZE, 0),
            serde_json::json!({"template": mismatched}).to_string(),
        ];
        let (base_url, requests) = paged_fixture_server(bodies).await;
        let error = fixture_client(base_url)
            .resolve_template(SPACE_ID, OBJECT_ID, "Starter")
            .await
            .expect_err("final type mismatch");
        assert!(matches!(error, AnytypeError::Other { .. }));
        assert_eq!(requests.await.expect("mismatch fixture").len(), 2);

        let mut unsafe_type = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        unsafe_type["type"]["id"] = serde_json::json!("../unsafe");
        let mut archived_type = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        archived_type["type"]["archived"] = serde_json::json!(true);
        let wrong_type_key = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            false,
            SPACE_ID,
            OTHER_ID,
            "page",
        );
        let archived_template = template_value(
            TEMPLATE_ID,
            Some("Starter"),
            true,
            SPACE_ID,
            OTHER_ID,
            "template",
        );
        for malformed in [
            unsafe_type,
            archived_type,
            wrong_type_key,
            archived_template,
        ] {
            let body = serde_json::json!({"template": malformed}).to_string();
            let (base_url, requests) = paged_fixture_server(vec![body]).await;
            let error = fixture_client(base_url)
                .resolve_template(SPACE_ID, OBJECT_ID, TEMPLATE_ID)
                .await
                .expect_err("unsafe direct template identity");
            assert!(matches!(error, AnytypeError::Other { .. }));
            assert_eq!(requests.await.expect("direct mismatch fixture").len(), 1);
        }
    }

    fn offline_client() -> AnytypeClient {
        // env keystore: no OS keyring or file access, suitable for offline tests
        let mut config = crate::client::ClientConfig::default().app_name("resolve-tests");
        config.keystore = Some("env".to_string());
        AnytypeClient::with_config(config).expect("client")
    }

    #[test]
    fn uppercase_detection() {
        assert!(starts_with_uppercase("Task"));
        assert!(!starts_with_uppercase("task"));
        assert!(!starts_with_uppercase(""));
        assert!(!starts_with_uppercase("@Task"));
    }

    #[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
    struct TestRow {
        id: String,
        name: String,
    }

    fn row(id: impl Into<String>, name: impl Into<String>) -> TestRow {
        TestRow {
            id: id.into(),
            name: name.into(),
        }
    }

    fn row_candidate(row: &TestRow) -> ResolveCandidate {
        ResolveCandidate::new(&row.id, &row.name)
    }

    #[test]
    fn match_classification_preserves_zero_and_one_items() {
        assert!(matches!(
            classify_matches(Vec::<TestRow>::new(), row_candidate),
            Ok(MatchClassification::None)
        ));

        let Ok(MatchClassification::Unique(one)) =
            classify_matches([row("id-1", "Only")], row_candidate)
        else {
            panic!("one stable id must resolve uniquely");
        };
        assert_eq!(one, row("id-1", "Only"));
    }

    #[test]
    fn duplicate_rows_for_one_stable_id_resolve_uniquely() {
        let Ok(MatchClassification::Unique(unique)) =
            classify_matches([row("id-1", "Work"), row("id-1", "work")], row_candidate)
        else {
            panic!("duplicate rows for one id must not be ambiguous");
        };
        assert_eq!(unique, row("id-1", "Work"));
    }

    #[test]
    fn domain_candidates_deduplicate_space_type_view_chat_and_property_rows() {
        let assert_unique = |candidates: Vec<ResolveCandidate>| {
            let mut accumulator = MatchAccumulator::new();
            for candidate in candidates {
                accumulator.push((), candidate);
            }
            assert!(matches!(
                accumulator.finish(),
                MatchClassification::Unique(())
            ));
        };

        let space: crate::spaces::Space = serde_json::from_value(serde_json::json!({
            "id": "space-a", "name": "Work", "object": "space",
            "description": null, "icon": null, "gateway_url": null, "network_id": null
        }))
        .unwrap();
        let typ: Type = serde_json::from_value(serde_json::json!({
            "archived": false, "id": "type-a", "key": "page", "name": "Page"
        }))
        .unwrap();
        let view = crate::views::View {
            filters: Vec::new(),
            id: "view-a".to_string(),
            layout: crate::views::ViewLayout::Grid,
            name: Some("Roadmap".to_string()),
            sorts: Vec::new(),
        };
        let chat: crate::objects::Object = serde_json::from_value(serde_json::json!({
            "archived": false, "id": "chat-a", "name": "General", "space_id": "space-a",
            "type": null
        }))
        .unwrap();
        let property: crate::properties::Property = serde_json::from_value(serde_json::json!({
            "id": "property-a", "key": "status", "name": "Status", "format": "text"
        }))
        .unwrap();

        assert_unique(vec![
            ResolveCandidate::new(&space.id, &space.name),
            ResolveCandidate::new(&space.id, &space.name),
        ]);
        assert_unique(vec![type_candidate(&typ), type_candidate(&typ)]);
        assert_unique(vec![view_candidate(&view), view_candidate(&view)]);
        assert_unique(vec![object_candidate(&chat), object_candidate(&chat)]);
        assert_unique(vec![
            property_candidate(&property),
            property_candidate(&property),
        ]);
    }

    #[test]
    fn safe_same_id_type_representative_wins_in_every_input_order() {
        let type_fixture = |id: &str, name: String| -> Type {
            serde_json::from_value(serde_json::json!({
                "archived": false,
                "id": id,
                "key": "page",
                "name": name,
            }))
            .unwrap()
        };
        let rows = [
            type_fixture("type-a", "A".repeat(MAX_RESOLVE_CANDIDATE_NAME_CHARS + 1)),
            type_fixture("type-a", "Zulu".to_string()),
            type_fixture("type-b", "Beta".to_string()),
        ];
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let ordered = order.map(|index| rows[index].clone());
            let Ok(MatchClassification::Ambiguous(candidates)) =
                classify_matches(ordered, type_candidate)
            else {
                panic!("two stable type ids must be ambiguous");
            };
            assert_eq!(
                candidates,
                vec![
                    ResolveCandidate::new("type-b", "Beta"),
                    ResolveCandidate::new("type-a", "Zulu"),
                ],
                "candidate choice changed for order {order:?}"
            );
        }
    }

    #[tokio::test]
    async fn type_and_property_key_resolution_bypass_cache_prime_paths() {
        let (base_url, request) = empty_page_server().await;
        let error = fixture_client(base_url)
            .resolve_type_id(SPACE_ID, "@page")
            .await
            .expect_err("empty fixture has no type");
        assert!(matches!(error, AnytypeError::NotFound { .. }));
        let request = request.await.expect("type fixture task");
        let request_line = request.lines().next().expect("type request line");
        assert!(
            request_line.starts_with(&format!("GET /v1/spaces/{SPACE_ID}/types?")),
            "unexpected type request: {request_line}"
        );
        assert!(request_line.contains("limit=99"), "{request_line}");

        let (base_url, request) = empty_page_server().await;
        let error = fixture_client(base_url)
            .resolve_property_id(SPACE_ID, "status")
            .await
            .expect_err("empty fixture has no property");
        assert!(matches!(error, AnytypeError::NotFound { .. }));
        let request = request.await.expect("property fixture task");
        let request_line = request.lines().next().expect("property request line");
        assert!(
            request_line.starts_with(&format!("GET /v1/spaces/{SPACE_ID}/properties?")),
            "unexpected property request: {request_line}"
        );
        assert!(request_line.contains("limit=99"), "{request_line}");
    }

    #[tokio::test]
    async fn explicit_type_id_resolution_uses_one_direct_get_with_cache_enabled() {
        for resolve_key in [false, true] {
            let (base_url, shutdown, traffic) = route_aware_type_server().await;
            let client = fixture_client(base_url);
            assert!(
                client.cache().is_enabled(),
                "fixture must exercise cache-on behavior"
            );

            if resolve_key {
                assert_eq!(
                    client
                        .resolve_type_key(SPACE_ID, OBJECT_ID)
                        .await
                        .expect("direct type key"),
                    "page"
                );
            } else {
                assert_eq!(
                    client
                        .resolve_type(SPACE_ID, OBJECT_ID)
                        .await
                        .expect("direct type")
                        .id,
                    OBJECT_ID
                );
            }

            shutdown.send(()).expect("stop route-aware type fixture");
            let traffic = traffic.await.expect("route-aware type fixture task");
            assert_eq!(traffic.type_list_pages, 0, "must not prime type cache");
            assert_eq!(traffic.direct_type_gets, 1);
            assert_eq!(traffic.requests.len(), 1);
            let request_line = traffic.requests[0]
                .lines()
                .next()
                .expect("type request line");
            assert_eq!(
                request_line,
                format!("GET /v1/spaces/{SPACE_ID}/types/{OBJECT_ID} HTTP/1.1")
            );
        }
    }

    #[tokio::test]
    async fn explicit_type_id_resolution_rejects_safe_mismatched_identity() {
        let body = serde_json::json!({
            "type": {
                "archived": false,
                "id": OTHER_OBJECT_ID,
                "key": "page",
                "name": "Page"
            }
        })
        .to_string();
        let (base_url, requests) = paged_fixture_server(vec![body]).await;
        let client = fixture_client(base_url);
        assert!(
            client.cache().is_enabled(),
            "fixture must exercise cache-on behavior"
        );

        let error = client
            .resolve_type(SPACE_ID, OBJECT_ID)
            .await
            .expect_err("a mismatched direct response must fail closed");
        let AnytypeError::Other { message } = &error else {
            panic!("identity mismatch must be an upstream error: {error}");
        };
        assert_eq!(message, "Anytype returned a mismatched type identity");
        let display = error.to_string();
        assert!(!display.contains(OBJECT_ID));
        assert!(!display.contains(OTHER_OBJECT_ID));

        let requests = requests.await.expect("mismatched type fixture task");
        assert_eq!(requests.len(), 1);
        let request_line = requests[0].lines().next().expect("type request line");
        assert_eq!(
            request_line,
            format!("GET /v1/spaces/{SPACE_ID}/types/{OBJECT_ID} HTTP/1.1")
        );
    }

    #[tokio::test]
    async fn public_space_resolution_deduplicates_across_http_pages() {
        let space = serde_json::json!({
            "id": "space-a", "name": "Work", "object": "space",
            "description": null, "icon": null, "gateway_url": null, "network_id": null
        });
        let first = serde_json::json!({
            "items": [space.clone()],
            "pagination": {"has_more": true, "limit": 99, "offset": 0, "total": 2}
        })
        .to_string();
        let second = serde_json::json!({
            "items": [space],
            "pagination": {"has_more": false, "limit": 99, "offset": 99, "total": 2}
        })
        .to_string();
        let (base_url, requests) = paged_fixture_server(vec![first, second]).await;

        let resolved = fixture_client(base_url)
            .resolve_space_id("Work")
            .await
            .expect("duplicate rows for one space id remain unique");
        assert_eq!(resolved, "space-a");
        let requests = requests.await.expect("space fixture task");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].lines().next().unwrap().contains("limit=99"));
        assert!(requests[1].lines().next().unwrap().contains("offset=99"));
    }

    #[tokio::test]
    async fn public_view_resolution_preserves_later_direct_id_across_pages() {
        let view = |id: &str, name: &str| {
            serde_json::json!({
                "filters": [], "id": id, "layout": "grid", "name": name, "sorts": []
            })
        };
        let first = serde_json::json!({
            "items": [view("view-a", "Roadmap"), view("view-b", "Roadmap")],
            "pagination": {"has_more": true, "limit": 99, "offset": 0, "total": 3}
        })
        .to_string();
        let second = serde_json::json!({
            "items": [view("Roadmap", "Different name")],
            "pagination": {"has_more": false, "limit": 99, "offset": 99, "total": 3}
        })
        .to_string();
        let (base_url, requests) = paged_fixture_server(vec![first, second]).await;

        let resolved = fixture_client(base_url)
            .resolve_view_id(SPACE_ID, OBJECT_ID, "Roadmap")
            .await
            .expect("later direct view id must outrank earlier duplicate names");
        assert_eq!(resolved, "Roadmap");
        let requests = requests.await.expect("view fixture task");
        assert_eq!(requests.len(), 2);
        assert!(requests[0].lines().next().unwrap().contains("limit=99"));
        assert!(requests[1].lines().next().unwrap().contains("offset=99"));
    }

    #[test]
    fn distinct_stable_ids_produce_deterministic_candidates() {
        let Ok(MatchClassification::Ambiguous(candidates)) =
            classify_matches([row("id-2", "alpha"), row("id-1", "Alpha")], row_candidate)
        else {
            panic!("two stable ids must be ambiguous");
        };
        assert_eq!(
            candidates,
            vec![
                ResolveCandidate::new("id-1", "Alpha"),
                ResolveCandidate::new("id-2", "alpha"),
            ]
        );
    }

    #[test]
    fn candidate_membership_is_independent_of_input_order() {
        let classify = |rows| {
            let Ok(MatchClassification::Ambiguous(candidates)) =
                classify_matches(rows, row_candidate)
            else {
                panic!("distinct stable ids must be ambiguous");
            };
            candidates
        };
        let forward = classify(vec![
            row("id-z", "Zed"),
            row("id-y", "Yankee"),
            row("id-a", "Alpha"),
        ]);
        let reverse = classify(vec![
            row("id-a", "Alpha"),
            row("id-y", "Yankee"),
            row("id-z", "Zed"),
        ]);

        assert_eq!(forward, reverse);
        assert_eq!(
            forward,
            vec![
                ResolveCandidate::new("id-a", "Alpha"),
                ResolveCandidate::new("id-y", "Yankee"),
                ResolveCandidate::new("id-z", "Zed"),
            ]
        );
    }

    #[test]
    fn match_classification_selects_deterministic_top_ten() {
        let rows: Vec<_> = (0..25)
            .rev()
            .map(|index| row(format!("id-{index:02}"), format!("Name {index:02}")))
            .collect();
        let Ok(MatchClassification::Ambiguous(candidates)) = classify_matches(rows, row_candidate)
        else {
            panic!("many stable ids must be ambiguous");
        };

        assert_eq!(candidates.len(), MAX_RESOLVE_CANDIDATES);
        assert_eq!(candidates[0].id(), "id-00");
        assert_eq!(candidates[9].id(), "id-09");
    }

    #[test]
    fn later_direct_view_id_wins_over_earlier_name_ambiguity() {
        let view = |id: &str, name: &str| crate::views::View {
            filters: Vec::new(),
            id: id.to_string(),
            layout: crate::views::ViewLayout::Grid,
            name: Some(name.to_string()),
            sorts: Vec::new(),
        };
        let mut matches = ViewMatchAccumulator::new("Roadmap");
        assert_eq!(matches.push(view("view-a", "Roadmap")), None);
        assert_eq!(matches.push(view("view-b", "Roadmap")), None);
        assert_eq!(
            matches.push(view("Roadmap", "Different name")),
            Some("Roadmap".to_string())
        );
    }

    #[test]
    fn candidate_selection_filters_invalid_values_before_its_cap() {
        let mut candidates = Vec::new();
        for index in 0..12 {
            insert_bounded_candidate(
                &mut candidates,
                ResolveCandidate::new(format!("bad/{index:02}"), format!("A {index:02}")),
            );
        }
        for index in (0..12).rev() {
            insert_bounded_candidate(
                &mut candidates,
                ResolveCandidate::new(format!("id-{index:02}"), format!("Valid {index:02}")),
            );
        }

        assert_eq!(candidates.len(), MAX_RESOLVE_CANDIDATES);
        assert_eq!(candidates[0].id(), "id-00");
        assert_eq!(candidates[9].id(), "id-09");
    }

    #[test]
    fn invalid_candidates_before_a_valid_match_do_not_hide_it() {
        let Ok(MatchClassification::Ambiguous(candidates)) = classify_matches(
            [
                row("bad/one", "A invalid"),
                row("bad/two", "B invalid"),
                row("id-3", "C valid"),
            ],
            row_candidate,
        ) else {
            panic!("distinct stable ids must remain ambiguous");
        };
        assert_eq!(candidates, vec![ResolveCandidate::new("id-3", "C valid")]);
    }

    #[test]
    fn unique_scans_fail_explicitly_beyond_the_hard_limit() {
        let rows = std::iter::repeat_with(|| row("id-1", "Only")).take(MAX_RESOLVE_SCAN_ITEMS + 1);
        assert!(matches!(
            classify_matches(rows, row_candidate),
            Err(ResolutionScanLimit)
        ));
    }

    #[test]
    fn ambiguous_scans_also_fail_when_candidate_completeness_exceeds_the_limit() {
        let rows = (0..=MAX_RESOLVE_SCAN_ITEMS)
            .map(|index| row(format!("id-{index}"), format!("Name {index}")));
        assert!(matches!(
            classify_matches(rows, row_candidate),
            Err(ResolutionScanLimit)
        ));
    }

    #[test]
    fn bare_chat_id_discovery_has_one_global_space_and_chat_budget() {
        let mut budget = ResolutionScanBudget::new();
        for _ in 0..400 {
            budget.record("chat", "chat-id").unwrap();
        }
        for _ in 0..600 {
            budget.record("chat", "chat-id").unwrap();
        }

        let error = budget.record("chat", "chat-id").unwrap_err();
        assert!(matches!(
            error,
            AnytypeError::ResolutionLimitExceeded {
                obj_type,
                key,
                limit: MAX_RESOLVE_SCAN_ITEMS,
            } if obj_type == "chat" && key == "chat-id"
        ));
    }

    #[tokio::test]
    async fn paged_type_and_property_scans_return_the_explicit_limit_error() {
        for obj_type in ["type", "property"] {
            let page = crate::paged::PagedResult::from_items(
                std::iter::repeat_with(|| row("id-1", "Only"))
                    .take(MAX_RESOLVE_SCAN_ITEMS + 1)
                    .collect(),
            );
            let result = scan_paged_matches(page, obj_type, "key", |_| true, row_candidate).await;
            let Err(AnytypeError::ResolutionLimitExceeded {
                obj_type: actual_type,
                key,
                limit,
            }) = result
            else {
                panic!("{obj_type} scan must return the explicit hard-limit error");
            };
            assert_eq!(actual_type, obj_type);
            assert_eq!(key, "key");
            assert_eq!(limit, MAX_RESOLVE_SCAN_ITEMS);
        }
    }

    #[test]
    fn ambiguity_error_exposes_only_bounded_candidates() {
        let error = ambiguous(
            "space",
            "Work",
            (0..12).map(|index| {
                ResolveCandidate::new(format!("id-{index:02}"), format!("Work {index:02}"))
            }),
        );

        let AnytypeError::Ambiguous {
            obj_type,
            key,
            candidates,
        } = &error
        else {
            panic!("expected ambiguity error");
        };
        assert_eq!(obj_type, "space");
        assert_eq!(key, "Work");
        assert_eq!(candidates.len(), MAX_RESOLVE_CANDIDATES);
        assert_eq!(error.resolve_candidates(), Some(candidates.as_slice()));
    }

    #[tokio::test]
    async fn space_id_passes_through() {
        let client = offline_client();
        let resolved = client.resolve_space_id(SPACE_ID).await.expect("space id");
        assert_eq!(resolved, SPACE_ID);
    }

    #[tokio::test]
    async fn type_key_at_prefix_offline() {
        let client = offline_client();
        let key = client
            .resolve_type_key(SPACE_ID, "@page")
            .await
            .expect("type key");
        assert_eq!(key, "page");
    }

    #[tokio::test]
    async fn type_id_passes_through() {
        let client = offline_client();
        let id = client
            .resolve_type_id(SPACE_ID, OBJECT_ID)
            .await
            .expect("type id");
        assert_eq!(id, OBJECT_ID);
    }

    #[tokio::test]
    async fn property_id_passes_through() {
        let client = offline_client();
        let id = client
            .resolve_property_id(SPACE_ID, OBJECT_ID)
            .await
            .expect("property id");
        assert_eq!(id, OBJECT_ID);
    }

    #[tokio::test]
    async fn chat_id_with_space_passes_through() {
        let client = offline_client();
        let target = client
            .resolve_chat_target(Some(SPACE_ID), OBJECT_ID)
            .await
            .expect("chat target");
        assert_eq!(
            target,
            ChatTarget {
                space_id: Some(SPACE_ID.to_string()),
                chat_id: OBJECT_ID.to_string(),
            }
        );
    }
}
