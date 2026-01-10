//! Paginated and Stream results for list and search methods.
//!
//! `PaginatedResponse<T>` fetches object lists in pages.
//!
//! `PagedResult<T>` wraps a `PaginatedResponse<T>` with methods to
//! fetch all pages as a stream [`to_stream()`](PagedResult::into_stream),
//! or collect them into a vector, with [`collect_all()`](PagedResult::collect_all).
//!
//!
use std::{fmt, ops::Deref, sync::Arc};

use futures::{
    StreamExt,
    stream::{BoxStream, unfold},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned, ser::Serializer};

use crate::{
    Result,
    error::AnytypeError,
    http_client::{HttpClient, HttpRequest},
};

/// A paginated result converted to a stream of all items.
///
/// `PagedResult<T>` wraps a `PaginatedResponse<T>` and retains the information
/// needed to fetch subsequent pages. It implements `Deref` to `PaginatedResponse<T>`,
/// so you can access `.items`, `.pagination`, `.len()`, etc. directly.
///
/// # Example
///
/// ```rust
/// use anytype::prelude::*;
/// use futures::StreamExt;
///
/// # async fn example() -> Result<(), AnytypeError> {
/// #   let client = AnytypeClient::new("doc test")?.env_key_store()?;
/// // Access first page directly via Deref
/// let result = client.spaces().list().await?;
/// println!("First page: {} items, total: {}", result.len(), result.pagination.total);
///
/// // Stream all items from all pages
/// let mut stream = client.spaces().list().await?.into_stream();
/// while let Some(space) = stream.next().await {
///     let space = space?;
///     println!("Space: {}", space.name);
/// }
///
/// // Or collect all items
/// let all_spaces = client.spaces().list().await?.collect_all().await?;
/// # Ok(())
/// # }
/// ```
pub struct PagedResult<T> {
    response: PaginatedResponse<T>,
    refill: Option<Refill>,
}

// client and request object needed to get next PaginatedResponse
#[derive(Clone)]
struct Refill {
    client: Arc<HttpClient>,
    request: HttpRequest,
}

impl<T> PagedResult<T> {
    /// Creates a new PagedResult from a response, client, and the original request.
    pub(crate) fn new(
        response: PaginatedResponse<T>,
        client: Arc<HttpClient>,
        request: HttpRequest,
    ) -> Self {
        Self {
            response,
            refill: Some(Refill { client, request }),
        }
    }

    /// Constructs a single-page response with all items, no client, and dummy request.
    fn single_page(response: PaginatedResponse<T>) -> Self {
        Self {
            response,
            refill: None,
        }
    }

    /// Creates a paged result from a complete list of items.
    /// Used when using cached items to provide a list() result.
    pub(crate) fn from_items(items: Vec<T>) -> Self {
        let total = items.len();
        let response = PaginatedResponse {
            items,
            pagination: PaginationMeta {
                has_more: false,
                limit: total,
                offset: 0,
                total,
            },
        };
        Self::single_page(response)
    }

    /// Consumes this result and returns the underlying `PaginatedResponse<T>`.
    pub fn into_response(self) -> PaginatedResponse<T> {
        self.response
    }
}

impl<T> Deref for PagedResult<T> {
    type Target = PaginatedResponse<T>;

    fn deref(&self) -> &Self::Target {
        &self.response
    }
}

// Implement Debug by delegating to the inner response
impl<T: fmt::Debug> fmt::Debug for PagedResult<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PagedResult")
            .field("response", &self.response)
            .finish()
    }
}

// Implement Serialize by delegating to the inner response
// This allows CLI code to serialize PagedResult<T> as if it were PaginatedResponse<T>
impl<T: Serialize> Serialize for PagedResult<T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.response.serialize(serializer)
    }
}

#[allow(clippy::type_complexity)]
fn next_response_iter<T>(
    next_response: PaginatedResponse<T>,
    limit: usize,
    refill: Refill,
) -> Option<(
    Result<T, AnytypeError>,
    (
        std::vec::IntoIter<T>,
        bool,
        usize,
        usize,
        Option<Refill>,
        bool,
    ),
)> {
    let new_has_more = next_response.pagination.has_more;
    let new_offset = next_response.pagination.offset + next_response.pagination.limit;
    let mut new_items = next_response.items.into_iter();

    // Get first item from new page (empty page stops iteration)
    new_items.next().map(|item| {
        (
            Ok(item),
            (
                new_items,
                new_has_more,
                new_offset,
                limit,
                Some(refill),
                // Some(Refill {
                //     client: refill.client.clone(),
                //     request: next_request,
                // }),
                false,
            ),
        )
    })
}

impl<T: DeserializeOwned + Send + 'static> PagedResult<T> {
    /// Converts this paginated result into a stream of all items across all pages.
    ///
    /// The stream yields items from the first page immediately, then fetches
    /// subsequent pages as needed when `has_more` is true.
    ///
    /// # Example
    ///
    /// ```rust
    /// use anytype::prelude::*;
    /// use futures::StreamExt;
    ///
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?.env_key_store()?;
    /// let mut stream = client.spaces().list().await?.into_stream();
    /// while let Some(space) = stream.next().await {
    ///     let space = space?;
    ///     println!("Space: {}", space.name);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn into_stream(self) -> BoxStream<'static, Result<T>> {
        let response = self.response;
        let refill = self.refill;

        // State for pagination
        let current_items = response.items.into_iter();
        let has_more = response.pagination.has_more;
        let offset = response.pagination.offset + response.pagination.limit;
        let limit = response.pagination.limit;

        // create stream from current_items and async closure
        unfold(
            (current_items, has_more, offset, limit, refill, false),
            move |(mut items, has_more, offset, limit, refill, mut errored)| async move {
                // If we've already errored, stop the stream
                if errored {
                    return None;
                }

                // Try to get next item from current page
                if let Some(item) = items.next() {
                    return Some((
                        Ok(item),
                        (items, has_more, offset, limit, refill.clone(), false),
                    ));
                }

                // Current page exhausted, fetch next page if available
                if has_more && let Some(refill) = refill {
                    // Build the next request with updated offset
                    let next_request = refill.request.with_pagination(offset, limit);
                    match refill
                        .client
                        .send::<PaginatedResponse<T>>(next_request.clone())
                        .await
                    {
                        Ok(next_response) => next_response_iter(
                            next_response,
                            limit,
                            Refill {
                                client: refill.client.clone(),
                                request: next_request,
                            },
                        ),
                        Err(e) => {
                            // Yield the error and mark as errored to stop on next iteration
                            errored = true;
                            Some((Err(e), (items, false, offset, limit, Some(refill), errored)))
                        }
                    }
                } else {
                    None
                }
            },
        )
        .boxed()
    }

    /// Collects all items from all pages into a vector.
    ///
    /// This is a convenience method that consumes the stream and collects all items.
    /// Stops on the first error encountered.
    ///
    /// # Example
    ///
    /// ```rust
    /// use anytype::prelude::*;
    ///
    /// # async fn example() -> Result<(), AnytypeError> {
    /// #   let client = AnytypeClient::new("doc test")?.env_key_store()?;
    /// let all_spaces = client.spaces().list().await?.collect_all().await?;
    /// println!("Total spaces: {}", all_spaces.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn collect_all(self) -> Result<Vec<T>> {
        let mut stream = self.into_stream();
        let mut items = Vec::new();

        while let Some(result) = stream.next().await {
            items.push(result?);
        }

        Ok(items)
    }
}

// Implement IntoIterator for the first page only (delegates to PaginatedResponse)
impl<'a, T> IntoIterator for &'a PagedResult<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.response.items.iter()
    }
}

/// Pagination information
/// For convenience, the limit and offset can be turned into Query with into()
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PaginationResponse {
    pub has_more: bool,
    pub limit: usize,
    pub offset: usize,
    pub total: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PaginatedResponse<T> {
    #[serde(default = "Vec::new", alias = "data")]
    pub items: Vec<T>,
    pub pagination: PaginationMeta,
}

impl<T> PaginatedResponse<T> {
    /// Returns the number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns true if there are no items in this page.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterates over the items in this response (may need to get next page for all).
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.items.iter()
    }

    /// Creates a mutable iterator over items in this response.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.items.iter_mut()
    }
}

// create iterator over a shared reference to items
impl<'a, T> IntoIterator for &'a PaginatedResponse<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    /// Returns an iterator over shared items.
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

/// pagination record keeping, returned as part of PaginatedResponse
#[derive(Debug, Deserialize, Serialize)]
pub struct PaginationMeta {
    pub has_more: bool,
    pub limit: usize,
    pub offset: usize,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use crate::{objects::Object, paged::*};

    /// Test data structure for pagination tests
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestItem {
        id: String,
        name: String,
    }

    /// Create a test HttpRequest
    fn create_test_request() -> HttpRequest {
        HttpRequest {
            method: reqwest::Method::GET,
            path: "/test".to_string(),
            query: vec![
                ("limit".to_string(), "10".to_string()),
                ("offset".to_string(), "0".to_string()),
            ],
            body: None,
        }
    }

    #[test]
    fn test_deref_to_paginated_response() {
        let items = vec![
            TestItem {
                id: "1".to_string(),
                name: "Item 1".to_string(),
            },
            TestItem {
                id: "2".to_string(),
                name: "Item 2".to_string(),
            },
        ];
        let paged = PagedResult::from_items(items.clone());

        // Test Deref - access items directly
        assert_eq!(paged.items.len(), 2);
        assert_eq!(paged.items[0].id, "1");
        assert_eq!(paged.items[1].name, "Item 2");

        // Test Deref - access pagination
        assert_eq!(paged.pagination.total, 2);
        assert!(!paged.pagination.has_more);
        assert_eq!(paged.pagination.offset, 0);
    }

    #[test]
    fn test_deref_len_and_is_empty() {
        let items = vec![TestItem {
            id: "1".to_string(),
            name: "Item 1".to_string(),
        }];
        let paged = PagedResult::from_items(items);

        // Test len() through Deref
        assert_eq!(paged.len(), 1);
        assert!(!paged.is_empty());

        // Test empty result
        let empty_paged = PagedResult::<Object>::from_items(vec![]);
        assert_eq!(empty_paged.len(), 0);
        assert!(empty_paged.is_empty());
    }

    #[test]
    fn test_debug_implementation() {
        let items = vec![TestItem {
            id: "1".to_string(),
            name: "Test".to_string(),
        }];
        let paged = PagedResult::from_items(items);

        let debug_str = format!("{:?}", paged);

        // Should contain PagedResult and response
        assert!(debug_str.contains("PagedResult"));
        assert!(debug_str.contains("response"));
        assert!(debug_str.contains("TestItem"));
    }

    #[test]
    fn test_serialize_implementation() {
        let items = vec![
            TestItem {
                id: "1".to_string(),
                name: "Item 1".to_string(),
            },
            TestItem {
                id: "2".to_string(),
                name: "Item 2".to_string(),
            },
        ];
        let paged = PagedResult::from_items(items);

        // Serialize should produce JSON of the inner PaginatedResponse
        let json = serde_json::to_string(&paged).expect("Failed to serialize");

        // Parse back and verify structure
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("Failed to parse JSON");

        assert!(parsed["items"].is_array());
        assert_eq!(parsed["items"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["items"][0]["id"], "1");
        assert_eq!(parsed["items"][1]["name"], "Item 2");
        assert_eq!(parsed["pagination"]["total"], 2);
        assert_eq!(parsed["pagination"]["has_more"], false);
    }

    #[test]
    fn test_into_iterator_for_reference() {
        let items = vec![
            TestItem {
                id: "1".to_string(),
                name: "Item 1".to_string(),
            },
            TestItem {
                id: "2".to_string(),
                name: "Item 2".to_string(),
            },
            TestItem {
                id: "3".to_string(),
                name: "Item 3".to_string(),
            },
        ];
        let paged = PagedResult::from_items(items.clone());

        // Iterate using for loop (uses IntoIterator for &PagedResult)
        let mut collected: Vec<&TestItem> = Vec::new();
        for item in &paged {
            collected.push(item);
        }

        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].id, "1");
        assert_eq!(collected[1].id, "2");
        assert_eq!(collected[2].id, "3");
    }

    #[test]
    fn test_iter_method() {
        let items = vec![
            TestItem {
                id: "1".to_string(),
                name: "Item 1".to_string(),
            },
            TestItem {
                id: "2".to_string(),
                name: "Item 2".to_string(),
            },
        ];
        let paged = PagedResult::from_items(items);

        // Use iter() through Deref
        let names: Vec<&str> = paged.iter().map(|item| item.name.as_str()).collect();

        assert_eq!(names, vec!["Item 1", "Item 2"]);
    }

    #[test]
    fn test_http_request_with_pagination() {
        let request = create_test_request();

        let new_request = request.with_pagination(20, 15);

        // Check that offset and limit are updated
        let offset = new_request.query.iter().find(|(k, _)| k == "offset");
        let limit = new_request.query.iter().find(|(k, _)| k == "limit");

        assert_eq!(offset, Some(&("offset".to_string(), "20".to_string())));
        assert_eq!(limit, Some(&("limit".to_string(), "15".to_string())));
    }

    #[test]
    fn test_http_request_with_pagination_replaces_existing() {
        let mut request = create_test_request();
        // Add some extra query params
        request
            .query
            .push(("filter".to_string(), "active".to_string()));

        let new_request = request.with_pagination(30, 25);

        // Should still have the filter param
        let filter = new_request.query.iter().find(|(k, _)| k == "filter");
        assert_eq!(filter, Some(&("filter".to_string(), "active".to_string())));

        // Offset and limit should be replaced, not duplicated
        let offsets: Vec<_> = new_request
            .query
            .iter()
            .filter(|(k, _)| k == "offset")
            .collect();
        let limits: Vec<_> = new_request
            .query
            .iter()
            .filter(|(k, _)| k == "limit")
            .collect();

        assert_eq!(offsets.len(), 1);
        assert_eq!(limits.len(), 1);
        assert_eq!(offsets[0].1, "30");
        assert_eq!(limits[0].1, "25");
    }

    #[tokio::test]
    async fn test_into_stream_single_page() {
        let items = vec![
            TestItem {
                id: "1".to_string(),
                name: "Item 1".to_string(),
            },
            TestItem {
                id: "2".to_string(),
                name: "Item 2".to_string(),
            },
        ];
        // has_more = false means single page
        let paged = PagedResult::from_items(items.clone());

        let mut stream = paged.into_stream();
        let mut collected = Vec::new();

        while let Some(result) = stream.next().await {
            collected.push(result.expect("Expected Ok item"));
        }

        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].id, "1");
        assert_eq!(collected[1].id, "2");
    }

    #[tokio::test]
    async fn test_into_stream_empty_page() {
        let paged = PagedResult::<Object>::from_items(vec![]);

        let mut stream = paged.into_stream();
        let mut count = 0;

        while let Some(result) = stream.next().await {
            result.expect("Expected Ok item");
            count += 1;
        }

        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_collect_all_single_page() {
        let items = vec![
            TestItem {
                id: "1".to_string(),
                name: "Item 1".to_string(),
            },
            TestItem {
                id: "2".to_string(),
                name: "Item 2".to_string(),
            },
            TestItem {
                id: "3".to_string(),
                name: "Item 3".to_string(),
            },
        ];
        let paged = PagedResult::from_items(items.clone());

        let all_items = paged
            .collect_all()
            .await
            .expect("collect_all should succeed");

        assert_eq!(all_items.len(), 3);
        assert_eq!(all_items[0].id, "1");
        assert_eq!(all_items[1].id, "2");
        assert_eq!(all_items[2].id, "3");
    }

    #[tokio::test]
    async fn test_collect_all_empty() {
        let paged = PagedResult::<Object>::from_items(vec![]);

        let all_items = paged
            .collect_all()
            .await
            .expect("collect_all should succeed");

        assert!(all_items.is_empty());
    }
}
