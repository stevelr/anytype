//! common functions for cli
//!

use anyhow::Result;
use anytype::prelude::*;
use anytype::validation::looks_like_object_id;

use crate::cli::AppContext;

/// get type by key or id
pub(crate) async fn resolve_type(
    ctx: &AppContext,
    space_id: &str,
    type_key_or_id: &str,
) -> Result<Type> {
    if looks_like_object_id(type_key_or_id) {
        return Ok(ctx.client.get_type(space_id, type_key_or_id).get().await?);
    }
    Ok(ctx
        .client
        .lookup_type_by_key(space_id, type_key_or_id)
        .await?)
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
    if looks_like_object_id(&key_or_id) {
        return Ok(key_or_id);
    }
    let typ = ctx.client.lookup_type_by_key(space_id, &key_or_id).await?;
    Ok(typ.id)
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
