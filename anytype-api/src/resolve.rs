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
//! - [`resolve_view_id`](AnytypeClient::resolve_view_id) - view name or id → view id
//! - [`resolve_property_id`](AnytypeClient::resolve_property_id) - property key or id → property id
//! - [`resolve_chat_target`](AnytypeClient::resolve_chat_target) - chat (or space) name or id → [`ChatTarget`]
//! - [`resolve_chat_ids`](AnytypeClient::resolve_chat_ids) - batch form of [`resolve_chat_target`](AnytypeClient::resolve_chat_target)
//! - [`resolve_chat_name`](AnytypeClient::resolve_chat_name) - chat id → display name
//!
//! Shared conventions:
//!
//! - A value that already looks like an object id
//!   ([`looks_like_object_id`](crate::validation::looks_like_object_id)) is
//!   passed through without a server round trip.
//! - For types, a leading `@` forces key interpretation (`@page` means the
//!   type with key `page`), and a value starting with an uppercase ascii
//!   letter is matched case-insensitively against type *names*.
//! - Name matches are case-insensitive and must be unique:
//!   no match returns [`AnytypeError::NotFound`], more than one match
//!   returns [`AnytypeError::Ambiguous`].
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

use crate::{
    Result, client::AnytypeClient, error::AnytypeError, types::Type,
    validation::looks_like_object_id,
};

/// Name of the default chat created in every space.
///
/// [`resolve_chat_target`](AnytypeClient::resolve_chat_target) falls back to
/// the chat with this name when given only a space.
pub const DEFAULT_CHAT_NAME: &str = "General";

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
        if looks_like_object_id(space_id_or_name) {
            return Ok(space_id_or_name.to_string());
        }

        let spaces = self.spaces().list().await?.collect_all().await?;
        let needle = space_id_or_name.to_lowercase();
        let matches: Vec<_> = spaces
            .into_iter()
            .filter(|space| space.name.to_lowercase() == needle)
            .collect();

        match matches.len() {
            0 => Err(not_found("space", space_id_or_name)),
            1 => Ok(matches[0].id.clone()),
            _ => Err(ambiguous("space", space_id_or_name)),
        }
    }

    /// Resolves a type key, name, or id into the full [`Type`].
    ///
    /// Accepts `@key` (explicit key), a type id, a Name (uppercase first
    /// letter, matched case-insensitively against type names), or a key.
    ///
    /// # Errors
    /// - [`AnytypeError::NotFound`] if nothing matches
    /// - [`AnytypeError::Ambiguous`] if more than one type matches
    pub async fn resolve_type(&self, space_id: &str, type_key_or_id: &str) -> Result<Type> {
        if let Some(stripped) = type_key_or_id.strip_prefix('@') {
            return self.lookup_type_by_key(space_id, stripped).await;
        }
        if looks_like_object_id(type_key_or_id) {
            return self.get_type(space_id, type_key_or_id).get().await;
        }
        if starts_with_uppercase(type_key_or_id) {
            return self.resolve_type_by_name(space_id, type_key_or_id).await;
        }
        let matches = self.lookup_types(space_id, type_key_or_id).await?;
        match matches.len() {
            0 => Err(not_found("type", type_key_or_id)),
            1 => Ok(matches[0].clone()),
            _ => Err(ambiguous("type", type_key_or_id)),
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
            let typ = self.lookup_type_by_key(space_id, stripped).await?;
            return Ok(typ.id);
        }
        if looks_like_object_id(&key_or_id) {
            return Ok(key_or_id);
        }
        if starts_with_uppercase(&key_or_id) {
            return Ok(self.resolve_type_by_name(space_id, &key_or_id).await?.id);
        }
        let matches = self.lookup_types(space_id, &key_or_id).await?;
        match matches.len() {
            0 => Err(not_found("type", &key_or_id)),
            1 => Ok(matches[0].id.clone()),
            _ => Err(ambiguous("type", &key_or_id)),
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
    /// a `@key` value is unwrapped without a server round trip.
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
            let typ = self.get_type(space_id, &key_or_name).get().await?;
            return Ok(typ.key);
        }
        if starts_with_uppercase(&key_or_name) {
            return Ok(self.resolve_type_by_name(space_id, &key_or_name).await?.key);
        }
        let matches = self.lookup_types(space_id, &key_or_name).await?;
        match matches.len() {
            0 => Err(not_found("type", &key_or_name)),
            1 => Ok(matches[0].key.clone()),
            _ => Err(ambiguous("type", &key_or_name)),
        }
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
        let views = self
            .list_views(space_id, list_id)
            .limit(200)
            .list()
            .await?
            .collect_all()
            .await?;

        if views.iter().any(|view| view.id == view_id_or_name) {
            return Ok(view_id_or_name.to_string());
        }

        let exact: Vec<_> = views
            .iter()
            .filter(|view| view.name.as_deref() == Some(view_id_or_name))
            .collect();
        if exact.len() == 1 {
            return Ok(exact[0].id.clone());
        }
        if exact.len() > 1 {
            return Err(ambiguous("view", view_id_or_name));
        }

        let needle = view_id_or_name.to_lowercase();
        let matches: Vec<_> = views
            .iter()
            .filter(|view| view.name.as_deref().unwrap_or("").to_lowercase() == needle)
            .collect();
        match matches.len() {
            0 => Err(not_found("view", view_id_or_name)),
            1 => Ok(matches[0].id.clone()),
            _ => Err(ambiguous("view", view_id_or_name)),
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
        let prop = self.lookup_property_by_key(space_id, &key_or_id).await?;
        Ok(prop.id)
    }

    /// Resolves a chat reference into a [`ChatTarget`].
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

    /// Resolves a chat name (case-insensitive) into a chat id within a space.
    async fn resolve_chat_id_in_space(
        &self,
        space_id: &str,
        chat_id_or_name: &str,
    ) -> Result<String> {
        let result = self
            .chats()
            .search_chats_in(space_id)
            .text(chat_id_or_name)
            .search()
            .await?;
        let needle = chat_id_or_name.to_lowercase();
        let matches: Vec<_> = result
            .items
            .into_iter()
            .filter(|chat| chat.name.as_deref().unwrap_or("").to_lowercase() == needle)
            .collect();

        match matches.len() {
            0 => Err(not_found("chat", chat_id_or_name)),
            1 => Ok(matches[0].id.clone()),
            _ => Err(ambiguous("chat", chat_id_or_name)),
        }
    }

    /// Returns the space id if a space with this id is accessible.
    async fn find_space_id_by_id(&self, space_id: &str) -> Result<Option<String>> {
        let spaces = self.spaces().list().await?.collect_all().await?;
        Ok(spaces
            .into_iter()
            .find(|space| space.id == space_id)
            .map(|space| space.id))
    }

    /// Returns true if a chat with this id is accessible.
    async fn chat_exists(&self, chat_id: &str) -> Result<bool> {
        let chats = self.chats().list_chats().list().await?;
        Ok(chats.items.iter().any(|chat| chat.id == chat_id))
    }

    /// Finds a space id by case-insensitive name match; `Ok(None)` when
    /// no space matches.
    async fn find_space_id_by_name(&self, space_name: &str) -> Result<Option<String>> {
        let spaces = self.spaces().list().await?.collect_all().await?;
        let needle = space_name.to_lowercase();
        let matches: Vec<_> = spaces
            .into_iter()
            .filter(|space| space.name.to_lowercase() == needle)
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches[0].id.clone())),
            _ => Err(ambiguous("space", space_name)),
        }
    }

    /// Resolves a type name (case-insensitive) into the full [`Type`].
    async fn resolve_type_by_name(&self, space_id: &str, name: &str) -> Result<Type> {
        let matches = self.lookup_types(space_id, name).await?;
        let needle = name.to_lowercase();
        let filtered: Vec<_> = matches
            .into_iter()
            .filter(|typ| typ.name.as_deref().unwrap_or("").to_lowercase() == needle)
            .collect();
        match filtered.len() {
            0 => Err(not_found("type", name)),
            1 => Ok(filtered[0].clone()),
            _ => Err(ambiguous("type", name)),
        }
    }
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

fn ambiguous(obj_type: &str, key: &str) -> AnytypeError {
    AnytypeError::Ambiguous {
        obj_type: obj_type.to_string(),
        key: key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // a valid space id (CID.HASH form) that passes looks_like_object_id
    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    // a valid bare CID object id (59 chars)
    const OBJECT_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";

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
