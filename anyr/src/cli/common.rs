//! common functions for cli
//!
//! Name and id resolution helpers (`resolve_space_id`, `resolve_type*`,
//! `resolve_chat_*`, `resolve_view_id`, `resolve_property_id`) live in the
//! anytype crate (`anytype::resolve`) as `AnytypeClient` methods.
//! This module keeps CLI display helpers only.

use std::collections::HashMap;

use anyhow::Result;
use anytype::prelude::*;

use crate::cli::AppContext;

pub struct MemberCache {
    identities: HashMap<String, String>,
}

pub async fn load_member_cache(ctx: &AppContext, space_id: &str) -> Result<MemberCache> {
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

pub fn resolve_member_name(space_id: &str, member_cache: &MemberCache, value: &str) -> String {
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
