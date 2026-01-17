//! # Anytype cache
//!
//! The cache is intended to save the number of messages to and from the server,
//! for metadata-type objects: spaces, properties, and types.
//!
//! Properties and types are indexed by id and key, so either is O(1) lookup.
//!
//! The cache enables several convenience functions, for example,
//! - lookup_property_by_key
//! - lookup_type_by_key
//!
//! Objects and other types are not cached.
//!
//! Caution: The cache does not detect updates to objects over the network,
//! (such as shared spaces) - only from clients. If your app expects frequent updates
//! for shared objects, you may want to periodically clear the cache.
//! (A potential resolution for this is under investigation: the gRPC api
//! has an event api for notification of changed objects)
//!

/*
 # Notes on Locking design:

 - No code ever tries to hold more than one mutex lock, so there is no risk of deadlock.

 - There are a few places where library code checks "cache.has_*", then, if false,
   fetches data to insert into the cache. This creates a slight chance of race condition,
   because a lock is not held across the data load, however, if the race condition
   does occur, the only cost would be extra fetches. With parallel operations, there is
   no risk of data integrity problems because cache updates are atomic. Since most
   expected use cases are single-threaded applications, this behavior seems reasonable
   for MVP.

 - We use non-poisoning parking_lot mutexes. If one thread crashes while holding
   a lock, the lock is released. This doesn't cause corruption because cache updates
   are effectively atomic:
    - Data preparation happens before acquiring the lock (see set_properties, set_types,
       set_spaces). If a panic occurs during .collect(), the lock was never held.
    - Each locked section performs exactly one mutation - assignment, insert,
       remove, clear, or take
    - No method holds a lock across multiple mutations - there's no code like
        "insert A, then insert B".
    If a panic occurs during HashMap::insert itself, there are bigger problems like memory
    corruption or something catastrophic

 - If multi-threaded uses were common, we could switch to tokio mutexes, which are also
   atomic but would require changing all the functions to async.
   Preferring to keep the simpler implementation until we learn of new use cases.
*/

use crate::{prelude::*, properties::Property};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::error;

/// Anytype cache for spaces, properties, and types
pub struct AnytypeCache {
    spaces: Mutex<Option<Vec<Space>>>,
    /// Properties indexed by both id and key (both point to the same Arc)
    properties: Mutex<HashMap<String, HashMap<String, Arc<Property>>>>,
    /// Types indexed by both id and key (both point to the same Arc)
    types: Mutex<HashMap<String, HashMap<String, Arc<Type>>>>,
    enabled: Mutex<bool>,
}

impl Default for AnytypeCache {
    fn default() -> Self {
        Self {
            enabled: Mutex::new(true),
            spaces: Mutex::new(None),
            properties: Mutex::new(HashMap::new()),
            types: Mutex::new(HashMap::new()),
        }
    }
}

impl AnytypeCache {
    /// Clears the entire cache.
    pub fn clear(&self) {
        self.clear_spaces();
        self.clear_properties(None);
        self.clear_types(None);
    }

    /// Enables cache
    /// Cache is always cleared if disabled and re-enabled, to ensure it's not stale
    pub fn enable(&self) {
        // clear _should be_ redundant here, since disabled caches should always be empty
        self.clear();
        *self.enabled.lock() = true;
    }

    /// disable and clear cache
    pub fn disable(&self) {
        // clear to ensure the cache doesn't hold stale data
        self.clear();
        *self.enabled.lock() = false;
    }

    /// returns true if the cache is enabled
    pub fn is_enabled(&self) -> bool {
        *self.enabled.lock()
    }

    /// Removes all cached properties and types for the space.
    /// Does not remove space from cached spaces
    pub fn clear_space_items(&self, space_id: &str) {
        self.clear_properties(Some(space_id));
        self.clear_types(Some(space_id));
    }

    /// Clears the cached spaces so the next list/get fetches from the API.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// # let client = AnytypeClient::new("my-app")?;
    /// client.cache().clear_spaces();
    /// # Ok(())
    /// # }
    /// ```
    pub fn clear_spaces(&self) {
        // To clear spaces cache, set to None (not Some(Vec::new())).
        self.spaces.lock().take();
    }

    /// Clears all cached properties for a space, or all spaces
    ///
    /// # Example
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// # let client = AnytypeClient::new("my-app")?;
    /// client.cache().clear_properties(None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn clear_properties(&self, space_id: Option<&str>) {
        let mut properties = self.properties.lock();
        if let Some(space_id) = space_id {
            properties.remove(space_id);
        } else {
            properties.clear();
        }
    }

    /// Clears the cached types so the next list/get fetches from the API.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use anytype::prelude::*;
    /// # fn example() -> Result<(), AnytypeError> {
    /// # let client = AnytypeClient::new("my-app")?;
    /// client.cache().clear_types(None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn clear_types(&self, space_id: Option<&str>) {
        let mut types = self.types.lock();
        if let Some(space_id) = space_id {
            types.remove(space_id);
        } else {
            types.clear();
        }
    }

    /// Returns a clone of spaces in the cache.
    pub(crate) fn spaces(&self) -> Option<Vec<Space>> {
        if self.is_enabled() {
            self.spaces.lock().clone()
        } else {
            None
        }
    }

    /// Replaces spaces in the cache. Used only by AnytypeClient.
    pub(crate) fn set_spaces(&self, spaces: Vec<Space>) {
        if self.is_enabled() {
            *self.spaces.lock() = Some(spaces);
        }
    }

    /// Returns true if we have a cached list of spaces.
    pub(crate) fn has_spaces(&self) -> bool {
        self.is_enabled() && self.spaces.lock().is_some()
    }

    /// Returns a space cloned from the cache.
    pub(crate) fn get_space(&self, space_id: &str) -> Option<Space> {
        if self.is_enabled() {
            self.spaces
                .lock()
                .as_ref()
                .and_then(|spaces| spaces.iter().find(|space| space.id == space_id).cloned())
        } else {
            None
        }
    }

    /// Returns an unsorted/unfiltered clone of all properties from a space in the cache.
    pub(crate) fn properties_for_space(&self, space_id: &str) -> Option<Vec<Property>> {
        if self.is_enabled() {
            self.properties.lock().get(space_id).map(|map| {
                // Deduplicate by Arc pointer since each property is stored twice (by id and key)
                let mut seen = HashSet::new();
                map.values()
                    .filter(|arc| seen.insert(Arc::as_ptr(arc)))
                    .map(|arc| (**arc).clone())
                    .collect()
            })
        } else {
            None
        }
    }

    /// Returns true if we have cached properties for the space.
    pub fn has_properties(&self, space_id: &str) -> bool {
        self.is_enabled() && self.properties.lock().contains_key(space_id)
    }

    /// Returns a property by id or key, if cached.
    pub(crate) fn get_property(&self, space_id: &str, id_or_key: &str) -> Option<Arc<Property>> {
        if self.is_enabled() {
            self.properties
                .lock()
                .get(space_id)
                .and_then(|properties| properties.get(id_or_key).cloned())
        } else {
            None
        }
    }

    /// Searches for cached properties using id, key, or name, with case-insensitive match.
    /// Returns None if cache is disabled or properties are not yet cached for this space
    pub fn lookup_property(
        &self,
        space_id: &str,
        text: impl AsRef<str>,
    ) -> Option<Vec<Arc<Property>>> {
        if self.is_enabled()
            && let Some(map) = self.properties.lock().get(space_id)
        {
            let check = text.as_ref().trim().to_lowercase();
            // Deduplicate by Arc pointer since each property is stored twice (by id and key)
            let mut seen = HashSet::new();
            Some(
                map.values()
                    .filter(|property| {
                        property.id == check
                            || property.key == check
                            || property.name.to_lowercase() == check
                    })
                    .filter(|arc| seen.insert(Arc::as_ptr(arc)))
                    .cloned()
                    .collect(),
            )
        } else {
            None
        }
    }

    /// Searches for cached properties using key.
    /// Keys are snake_case and lowercase. The parameter will be converted to lowercase.
    pub fn lookup_property_by_key(
        &self,
        space_id: &str,
        text: impl AsRef<str>,
    ) -> Option<Arc<Property>> {
        if self.is_enabled()
            && let Some(map) = self.properties.lock().get(space_id)
        {
            // Direct lookup by key (keys are indexed in the map)
            let check = text.as_ref().trim().to_lowercase();
            map.get(&check).cloned()
        } else {
            None
        }
    }

    /// Replaces cached properties for a space.
    /// Properties are indexed by both id and key for fast lookup.
    pub(crate) fn set_properties(&self, space_id: &str, properties: Vec<Property>) {
        if !self.is_enabled() {
            return;
        }
        // Each property is stored twice (by id and key), so allocate accordingly
        let mut map = HashMap::with_capacity(properties.len() * 2);
        for property in properties {
            if map.contains_key(&property.id) {
                error!(
                    space_id,
                    property_id = property.id.as_str(),
                    "duplicate property id in cache update"
                );
            }
            let arc = Arc::new(property);
            map.insert(arc.id.clone(), Arc::clone(&arc));
            map.insert(arc.key.clone(), arc);
        }
        self.properties.lock().insert(space_id.to_string(), map);
    }

    /// set or update property, if we have already cached properties for the space
    pub(crate) fn set_property(&self, space_id: &str, property: Property) {
        if self.is_enabled() && self.has_properties(space_id) {
            let mut props_lock = self.properties.lock();
            if let Some(space_props) = props_lock.get_mut(space_id) {
                let arc = Arc::new(property);
                space_props.insert(arc.id.clone(), Arc::clone(&arc));
                space_props.insert(arc.key.clone(), arc);
            }
        }
    }

    /// delete property from the cache (removes both id and key entries)
    pub(crate) fn delete_property(&self, space_id: &str, property_id: &str) {
        if self.is_enabled() {
            let mut props_lock = self.properties.lock();
            if let Some(space_props) = props_lock.get_mut(space_id) {
                // Look up to get both id and key, then remove both
                if let Some(prop) = space_props.get(property_id).cloned() {
                    space_props.remove(&prop.id);
                    space_props.remove(&prop.key);
                }
            }
        }
    }

    /// Returns an unsorted/unfiltered clone of all types from a space in the cache.
    pub(crate) fn types_for_space(&self, space_id: &str) -> Option<Vec<Type>> {
        if self.is_enabled() {
            self.types.lock().get(space_id).map(|map| {
                // Deduplicate by Arc pointer since each type is stored twice (by id and key)
                let mut seen = HashSet::new();
                map.values()
                    .filter(|arc| seen.insert(Arc::as_ptr(arc)))
                    .map(|arc| (**arc).clone())
                    .collect()
            })
        } else {
            None
        }
    }

    /// Searches for cached types using id, key, name, or plural name, with case-insensitive match.
    /// Excludes archived types
    // [ss]: don't know if these are guaranteed to be unique, so returning Vec for now
    pub fn lookup_types(&self, space_id: &str, text: impl AsRef<str>) -> Option<Vec<Arc<Type>>> {
        if self.is_enabled()
            && let Some(map) = self.types.lock().get(space_id)
        {
            let check = text.as_ref().trim().to_lowercase();
            // Deduplicate by Arc pointer since each type is stored twice (by id and key)
            let mut seen = HashSet::new();
            Some(
                map.values()
                    .filter(|type_| {
                        // check for !archived is redundant here because set_types()
                        // removes archived types before adding, but leaving the condition
                        // here because it's cheap and will still work even if set_types changes
                        !type_.archived
                            && (type_.id == check
                                || type_.key == check
                                || type_.name.as_deref().unwrap_or("").to_lowercase() == check
                                || type_.plural_name.as_deref().unwrap_or("").to_lowercase()
                                    == check)
                    })
                    .filter(|arc| seen.insert(Arc::as_ptr(arc)))
                    .cloned()
                    .collect(),
            )
        } else {
            None
        }
    }

    /// Searches for cached type by key.
    /// Keys are snake_case and lowercase. The parameter will be converted to lowercase.
    /// Excludes archived types.
    pub fn lookup_type_by_key(&self, space_id: &str, text: impl AsRef<str>) -> Option<Arc<Type>> {
        if self.is_enabled()
            && let Some(map) = self.types.lock().get(space_id)
        {
            // Direct lookup by key (keys are indexed in the map)
            let check = text.as_ref().trim().to_lowercase();
            map.get(&check).filter(|t| !t.archived).cloned()
        } else {
            None
        }
    }

    /// Returns true if we have types cached for the space.
    pub(crate) fn has_types(&self, space_id: &str) -> bool {
        self.is_enabled() && self.types.lock().contains_key(space_id)
    }

    /// Returns a cached type by id or key.
    pub(crate) fn get_type(&self, space_id: &str, id_or_key: &str) -> Option<Arc<Type>> {
        if self.is_enabled() {
            self.types
                .lock()
                .get(space_id)
                .and_then(|types| types.get(id_or_key).cloned())
        } else {
            None
        }
    }

    /// Replaces (or sets) types cached for a space.
    /// Removes archived types before caching.
    /// Types are indexed by both id and key for fast lookup.
    pub(crate) fn set_types(&self, space_id: &str, types: Vec<Type>) {
        if !self.is_enabled() {
            return;
        }
        let non_archived: Vec<_> = types.into_iter().filter(|t| !t.archived).collect();
        // Each type is stored twice (by id and key), so allocate accordingly
        let mut map = HashMap::with_capacity(non_archived.len() * 2);
        for typ in non_archived {
            let arc = Arc::new(typ);
            map.insert(arc.id.clone(), Arc::clone(&arc));
            map.insert(arc.key.clone(), arc);
        }
        self.types.lock().insert(space_id.to_string(), map);
    }

    /// set or update type, if we have already cached types for the space
    pub(crate) fn set_type(&self, space_id: &str, typ: Type) {
        if self.is_enabled() && self.has_types(space_id) {
            let mut types_lock = self.types.lock();
            if let Some(space_types) = types_lock.get_mut(space_id) {
                let arc = Arc::new(typ);
                space_types.insert(arc.id.clone(), Arc::clone(&arc));
                space_types.insert(arc.key.clone(), arc);
            }
        }
    }

    /// delete type from cache (removes both id and key entries)
    pub(crate) fn delete_type(&self, space_id: &str, type_id: &str) {
        if self.is_enabled() {
            let mut types_lock = self.types.lock();
            if let Some(space_types) = types_lock.get_mut(space_id) {
                // Look up to get both id and key, then remove both
                if let Some(typ) = space_types.get(type_id).cloned() {
                    space_types.remove(&typ.id);
                    space_types.remove(&typ.key);
                }
            }
        }
    }
}

impl AnytypeCache {
    /// Returns the number of spaces in the cache.
    #[doc(hidden)]
    pub fn num_spaces(&self) -> usize {
        if self.is_enabled() {
            self.spaces.lock().as_ref().map_or(0, Vec::len)
        } else {
            0
        }
    }

    /// Returns the number of properties in the cache.
    #[doc(hidden)]
    pub fn num_properties(&self) -> usize {
        if self.is_enabled() {
            self.properties
                .lock()
                .values()
                .map(|map| {
                    // Each property is stored twice (by id and key), count unique Arc pointers
                    map.values().map(Arc::as_ptr).collect::<HashSet<_>>().len()
                })
                .sum()
        } else {
            0
        }
    }

    /// Returns the number of types in the cache.
    #[doc(hidden)]
    pub fn num_types(&self) -> usize {
        if self.is_enabled() {
            self.types
                .lock()
                .values()
                .map(|map| {
                    // Each type is stored twice (by id and key), count unique Arc pointers
                    map.values().map(Arc::as_ptr).collect::<HashSet<_>>().len()
                })
                .sum()
        } else {
            0
        }
    }
}

impl std::fmt::Debug for AnytypeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let spaces_keys = self
            .spaces
            .lock()
            .as_ref()
            .map(|spaces| {
                spaces
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<&str>>()
                    .join(",")
            })
            .unwrap_or_default();

        f.debug_struct("AnytypeCache")
            .field("enabled", &self.is_enabled())
            .field("spaces", &format!("keys: {}", &spaces_keys))
            .field("properties", &format!("count: {}", self.num_properties()))
            .field("types", &format!("count: {}", self.num_types()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::AnytypeCache;
    use crate::prelude::*;

    fn sample_property(id: &str, key: &str) -> Property {
        serde_json::from_value(json!({
            "name": format!("prop {key}"),
            "key": key,
            "id": id,
            "format": "text",
            "tags": null
        }))
        .expect("property fixture")
    }

    fn sample_type(id: &str, key: &str) -> Type {
        serde_json::from_value(json!({
            "archived": false,
            "icon": null,
            "id": id,
            "key": key,
            "layout": "basic",
            "name": format!("{key} type"),
            "plural_name": format!("{key} types"),
            "properties": [sample_property("prop-1", "title")]
        }))
        .expect("type fixture")
    }

    fn sample_space(id: &str, name: &str) -> Space {
        serde_json::from_value(json!({
            "id": id,
            "name": name,
            "object": "space",
            "description": null,
            "icon": null,
            "gateway_url": null,
            "network_id": null
        }))
        .expect("space fixture")
    }

    #[test]
    fn test_cache_counts_and_clear() {
        let cache = AnytypeCache::default();

        cache.set_properties("space-a", vec![sample_property("p1", "title")]);
        cache.set_properties("space-b", vec![sample_property("p2", "status")]);
        cache.set_types("space-a", vec![sample_type("t1", "page")]);
        cache.set_types("space-b", vec![sample_type("t2", "task")]);
        cache.set_spaces(vec![
            sample_space("s1", "Space One"),
            sample_space("s2", "Space Two"),
        ]);

        assert_eq!(cache.num_properties(), 2);
        assert_eq!(cache.num_types(), 2);
        assert_eq!(cache.num_spaces(), 2);

        cache.clear_properties(Some("space-a"));
        assert_eq!(cache.num_properties(), 1);
        assert!(!cache.has_properties("space-a"));
        assert!(cache.has_properties("space-b"));

        cache.clear_types(None);
        assert_eq!(cache.num_types(), 0);

        cache.clear_spaces();
        assert_eq!(cache.num_spaces(), 0);
    }

    #[test]
    fn test_cache_disable_prevents_writes() {
        let cache = AnytypeCache::default();

        cache.disable();
        cache.set_properties("space-a", vec![sample_property("p1", "title")]);
        cache.set_types("space-a", vec![sample_type("t1", "page")]);
        cache.set_spaces(vec![sample_space("s1", "Space One")]);

        assert_eq!(cache.num_properties(), 0);
        assert_eq!(cache.num_types(), 0);
        assert_eq!(cache.num_spaces(), 0);

        cache.enable();
        cache.set_properties("space-a", vec![sample_property("p1", "title")]);
        assert_eq!(cache.num_properties(), 1);
    }

    #[test]
    fn test_cache_lookup_property_and_type() {
        let cache = AnytypeCache::default();

        cache.set_properties("space-a", vec![sample_property("p1", "status")]);
        cache.set_types("space-a", vec![sample_type("t1", "page")]);

        let prop = cache
            .lookup_property_by_key("space-a", "status")
            .expect("property lookup");
        assert_eq!(prop.id, "p1");

        let types = cache.lookup_types("space-a", "page").expect("type lookup");
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].id, "t1");
    }
}
