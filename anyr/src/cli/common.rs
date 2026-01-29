//! common functions for cli
//!

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use anytype::prelude::*;
use anytype::validation::looks_like_object_id;

use crate::cli::AppContext;

const DEFAULT_CHAT_NAME: &str = "General";

/// resolve space name or id into space id
pub(crate) async fn resolve_space_id(ctx: &AppContext, space_id_or_name: &str) -> Result<String> {
    if looks_like_object_id(space_id_or_name) {
        return Ok(space_id_or_name.to_string());
    }

    let spaces = ctx.client.spaces().list().await?.collect_all().await?;
    let needle = space_id_or_name.to_lowercase();
    let matches: Vec<_> = spaces
        .into_iter()
        .filter(|space| space.name.to_lowercase() == needle)
        .collect();

    match matches.len() {
        0 => Err(anyhow!("space not found: {}", space_id_or_name)),
        1 => Ok(matches[0].id.clone()),
        _ => Err(anyhow!("space name is ambiguous: {}", space_id_or_name)),
    }
}

pub(crate) async fn resolve_chat_target(
    ctx: &AppContext,
    space_id: Option<&str>,
    chat_id_or_name: &str,
) -> Result<(Option<String>, String)> {
    if let Some(space_id) = space_id {
        let chat_id = if looks_like_object_id(chat_id_or_name) {
            chat_id_or_name.to_string()
        } else {
            resolve_chat_id_in_space(ctx, space_id, chat_id_or_name).await?
        };
        return Ok((Some(space_id.to_string()), chat_id));
    }

    if looks_like_object_id(chat_id_or_name) {
        if chat_exists(ctx, chat_id_or_name).await? {
            return Ok((None, chat_id_or_name.to_string()));
        }
        if let Some(space_id) = find_space_id_by_id(ctx, chat_id_or_name).await? {
            let chat_id = resolve_chat_id_in_space(ctx, &space_id, DEFAULT_CHAT_NAME).await?;
            return Ok((Some(space_id), chat_id));
        }
        return Ok((None, chat_id_or_name.to_string()));
    }

    if let Some(space_id) = find_space_id_by_name(ctx, chat_id_or_name).await? {
        let chat_id = resolve_chat_id_in_space(ctx, &space_id, DEFAULT_CHAT_NAME).await?;
        return Ok((Some(space_id), chat_id));
    }

    Err(anyhow!(
        "chat name requires --space (or a space name/id) to resolve: {}",
        chat_id_or_name
    ))
}

pub(crate) async fn resolve_chat_ids(
    ctx: &AppContext,
    space_id: Option<&str>,
    chats: &[String],
) -> Result<Vec<String>> {
    let mut resolved = Vec::with_capacity(chats.len());
    for chat in chats {
        let (_, chat_id) = resolve_chat_target(ctx, space_id, chat).await?;
        resolved.push(chat_id);
    }
    Ok(resolved)
}

pub(crate) async fn resolve_chat_name(
    ctx: &AppContext,
    space_id: Option<&str>,
    chat_id: &str,
) -> Result<String> {
    if let Some(space_id) = space_id {
        let chat = ctx.client.chats().get_chat(space_id, chat_id).get().await?;
        return Ok(chat.name.unwrap_or_else(|| chat_id.to_string()));
    }

    let chats = ctx.client.chats().list_chats().list().await?;
    let chat = chats
        .items
        .into_iter()
        .find(|chat| chat.id == chat_id)
        .ok_or_else(|| anyhow!("chat not found: {}", chat_id))?;
    Ok(chat.name.unwrap_or_else(|| chat_id.to_string()))
}

async fn resolve_chat_id_in_space(
    ctx: &AppContext,
    space_id: &str,
    chat_id_or_name: &str,
) -> Result<String> {
    let result = ctx
        .client
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
        0 => Err(anyhow!("chat not found: {}", chat_id_or_name)),
        1 => Ok(matches[0].id.clone()),
        _ => Err(anyhow!("chat name is ambiguous: {}", chat_id_or_name)),
    }
}

async fn find_space_id_by_id(ctx: &AppContext, space_id: &str) -> Result<Option<String>> {
    let spaces = ctx.client.spaces().list().await?.collect_all().await?;
    Ok(spaces
        .into_iter()
        .find(|space| space.id == space_id)
        .map(|space| space.id))
}

async fn chat_exists(ctx: &AppContext, chat_id: &str) -> Result<bool> {
    let chats = ctx.client.chats().list_chats().list().await?;
    Ok(chats.items.iter().any(|chat| chat.id == chat_id))
}

async fn find_space_id_by_name(ctx: &AppContext, space_name: &str) -> Result<Option<String>> {
    let spaces = ctx.client.spaces().list().await?.collect_all().await?;
    let needle = space_name.to_lowercase();
    let matches: Vec<_> = spaces
        .into_iter()
        .filter(|space| space.name.to_lowercase() == needle)
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0].id.clone())),
        _ => Err(anyhow!("space name is ambiguous: {}", space_name)),
    }
}

/// get type by key or id
pub(crate) async fn resolve_type(
    ctx: &AppContext,
    space_id: &str,
    type_key_or_id: &str,
) -> Result<Type> {
    if let Some(stripped) = type_key_or_id.strip_prefix('@') {
        return Ok(ctx.client.lookup_type_by_key(space_id, stripped).await?);
    }
    if looks_like_object_id(type_key_or_id) {
        return Ok(ctx.client.get_type(space_id, type_key_or_id).get().await?);
    }
    if starts_with_uppercase(type_key_or_id) {
        return resolve_type_by_name(ctx, space_id, type_key_or_id).await;
    }
    let matches = ctx.client.lookup_types(space_id, type_key_or_id).await?;
    match matches.len() {
        1 => Ok(matches[0].clone()),
        _ => Err(anyhow!("type name is ambiguous: {}", type_key_or_id)),
    }
}

/// resolve array of types (ids or keys) into array of type ids
pub(crate) async fn resolve_type_ids(
    ctx: &AppContext,
    space_id: &str,
    types: &[String],
) -> Result<Vec<String>> {
    let mut resolved = Vec::with_capacity(types.len());
    for type_key in types {
        resolved.push(resolve_type_id(ctx, space_id, type_key).await?);
    }
    Ok(resolved)
}

/// resolve array of types (ids or keys) into array of type ids
pub(crate) async fn resolve_type_id(
    ctx: &AppContext,
    space_id: &str,
    key_or_id: impl Into<String>,
) -> Result<String> {
    let key_or_id = key_or_id.into();
    if let Some(stripped) = key_or_id.strip_prefix('@') {
        let typ = ctx.client.lookup_type_by_key(space_id, stripped).await?;
        return Ok(typ.id);
    }
    if looks_like_object_id(&key_or_id) {
        return Ok(key_or_id);
    }
    if starts_with_uppercase(&key_or_id) {
        return Ok(resolve_type_by_name(ctx, space_id, &key_or_id).await?.id);
    }
    let matches = ctx.client.lookup_types(space_id, &key_or_id).await?;
    match matches.len() {
        1 => Ok(matches[0].id.clone()),
        _ => Err(anyhow!("type name is ambiguous: {}", key_or_id)),
    }
}

/// resolve type name, key, or id into type key
pub(crate) async fn resolve_type_key(
    ctx: &AppContext,
    space_id: &str,
    key_or_name: impl Into<String>,
) -> Result<String> {
    let key_or_name = key_or_name.into();
    if let Some(stripped) = key_or_name.strip_prefix('@') {
        return Ok(stripped.to_string());
    }
    if looks_like_object_id(&key_or_name) {
        let typ = ctx.client.get_type(space_id, &key_or_name).get().await?;
        return Ok(typ.key);
    }
    if starts_with_uppercase(&key_or_name) {
        return Ok(resolve_type_by_name(ctx, space_id, &key_or_name).await?.key);
    }
    let matches = ctx.client.lookup_types(space_id, &key_or_name).await?;
    match matches.len() {
        1 => Ok(matches[0].key.clone()),
        _ => Err(anyhow!("type name is ambiguous: {}", key_or_name)),
    }
}

async fn resolve_type_by_name(ctx: &AppContext, space_id: &str, name: &str) -> Result<Type> {
    let matches = ctx.client.lookup_types(space_id, name).await?;
    let needle = name.to_lowercase();
    let filtered: Vec<_> = matches
        .into_iter()
        .filter(|typ| typ.name.as_deref().unwrap_or("").to_lowercase() == needle)
        .collect();
    match filtered.len() {
        0 => Err(anyhow!("type not found: {}", name)),
        1 => Ok(filtered[0].clone()),
        _ => Err(anyhow!("type name is ambiguous: {}", name)),
    }
}

fn starts_with_uppercase(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_uppercase())
}

pub(crate) struct MemberCache {
    identities: HashMap<String, String>,
}

pub(crate) async fn load_member_cache(ctx: &AppContext, space_id: &str) -> Result<MemberCache> {
    let members = ctx
        .client
        .members(space_id)
        .list()
        .await?
        .collect_all()
        .await?;
    Ok(MemberCache {
        identities: build_member_identity_map(&members),
    })
}

pub(crate) fn resolve_member_name(
    space_id: &str,
    member_cache: &MemberCache,
    value: &str,
) -> String {
    if let Some(name) = member_cache.identities.get(value) {
        return name.clone();
    }
    let Some(identity) = parse_member_identity(space_id, value) else {
        return value.to_string();
    };

    if let Some(name) = member_cache.identities.get(identity) {
        return name.clone();
    }

    identity.chars().take(8).collect()
}

fn build_member_identity_map(members: &[Member]) -> HashMap<String, String> {
    let mut identities = HashMap::new();
    for member in members {
        if let Some(identity) = member.identity.as_deref() {
            identities.insert(identity.to_string(), member.display_name().to_string());
        }
    }
    identities
}

fn parse_member_identity<'a>(space_id: &str, value: &'a str) -> Option<&'a str> {
    let space_fragment = space_id.replace('.', "_");
    let prefix = format!("_participant_{space_fragment}_");
    let identity = value.strip_prefix(&prefix)?;
    if identity.len() == 48 {
        Some(identity)
    } else {
        None
    }
}

/// resolve view name or id into view id for a list/type
pub(crate) async fn resolve_view_id(
    ctx: &AppContext,
    space_id: &str,
    list_id: &str,
    view_id_or_name: &str,
) -> Result<String> {
    let views = ctx
        .client
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
        return Err(anyhow!("view name is ambiguous: {}", view_id_or_name));
    }

    let needle = view_id_or_name.to_lowercase();
    let matches: Vec<_> = views
        .iter()
        .filter(|view| view.name.as_deref().unwrap_or("").to_lowercase() == needle)
        .collect();
    match matches.len() {
        1 => Ok(matches[0].id.clone()),
        0 => Err(anyhow!("view not found: {}", view_id_or_name)),
        _ => Err(anyhow!("view name is ambiguous: {}", view_id_or_name)),
    }
}

/// turn property key or id into id
pub(crate) async fn resolve_property_id(
    ctx: &AppContext,
    space_id: &str,
    key_or_id: impl Into<String>,
) -> Result<String> {
    let key_or_id = key_or_id.into();
    if looks_like_object_id(&key_or_id) {
        return Ok(key_or_id);
    }
    let prop = ctx
        .client
        .lookup_property_by_key(space_id, &key_or_id)
        .await?;
    Ok(prop.id)
}
