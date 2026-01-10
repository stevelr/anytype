//! Validation functions
//!

use snafu::prelude::*;

use crate::{
    Result,
    config::{
        VALIDATION_MARKDOWN_MAX_LEN, VALIDATION_MAX_QUERY_LEN, VALIDATION_NAME_MAX_LEN,
        VALIDATION_OID_MAX_LEN, VALIDATION_OID_MIN_LEN, VALIDATION_TAG_MAX_COUNT,
        VALIDATION_TAG_MAX_LEN,
    },
    prelude::*,
};

fn is_cid_chars(s: &str) -> bool {
    // base32 lower-case alphabet used by CIDv1 (no padding)
    s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'2'..=b'7'))
}
fn is_base36_chars(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9'))
}

/// Determine if a string is (probably) an object id or space id,
/// using syntactic checks.
/// Does not check whether the apparent-id represents an actual object.
pub fn looks_like_object_id(s: &str) -> bool {
    const PREFIX: &str = "bafyrei";
    const LEN: usize = 59;

    if s.len() < LEN || !s.starts_with(PREFIX) {
        return false;
    }
    if s.len() == LEN && is_cid_chars(s) {
        return true;
    }

    // space id is in the form CID.HASH, where CID is the 59-char base32 id,
    // and HASH is FNV‑1 64‑bit hash of the account public key bytes, formatted with base36
    if let Some((p1, p2)) = s.split_once('.')
        && p1.len() == LEN
        && is_cid_chars(p1)
        && matches!(p2.len(), 1..=13)
        && is_base36_chars(p2)
    {
        return true;
    }
    false
}

/// Validation limits for safety & sanity checking.
/// The objective is to catch requests that might cause resource exhaustion
/// such as server running out of memory. Regardless of whether the cause is
/// unintentional programming error, or intentional attack, sanity checks can be helpful.
/// A too-strict limit may cause the program to fail with legitimate inputs, so
/// it may be preferable to err on the side of looser limits.
/// All limits can be adjusted at client creation time
#[derive(Debug, Clone)]
pub struct ValidationLimits {
    /// max size of markdown in bytes
    pub markdown_max_len: u64,

    /// max length of an object name in bytes
    pub name_max_len: u64,

    /// max number of tags
    pub tag_max_count: u64,

    /// max length of a tag
    pub tag_max_len: u64,

    /// minimum length of object id
    pub oid_min_len: u64,

    /// max length of object id
    pub oid_max_len: u64,

    /// max size of a query (total length of key=value params)
    pub max_query_len: u64,
}

impl Default for ValidationLimits {
    fn default() -> Self {
        ValidationLimits {
            // max size of markdown (body) (default: 10 MiB)
            markdown_max_len: VALIDATION_MARKDOWN_MAX_LEN,
            // max length of object name (default: 4096 bytes)
            name_max_len: VALIDATION_NAME_MAX_LEN,
            // max number of tags per object (default: 4096 tags)
            tag_max_count: VALIDATION_TAG_MAX_COUNT,
            // max size of a tag string (default: 1024)
            tag_max_len: VALIDATION_TAG_MAX_LEN,
            // min length of an object id (default: 20 B)
            oid_min_len: VALIDATION_OID_MIN_LEN,
            // max length of an object id (default: 200 B)
            oid_max_len: VALIDATION_OID_MAX_LEN,
            // max size of query string (approximate) (default: 4000 bytes)
            max_query_len: VALIDATION_MAX_QUERY_LEN,
        }
    }
}

impl ValidationLimits {
    /// Checks an object id: not empty, and length within expected range
    #[doc(hidden)]
    pub fn validate_id(&self, id: &str, description: &str) -> Result<()> {
        ensure!(
            !id.is_empty(),
            ValidationSnafu {
                message: format!("{description} id cannot be empty"),
            }
        );
        // looks_like_object_id checks the length, prefix, and character set
        ensure!(
            looks_like_object_id(id),
            ValidationSnafu {
                message: format!("{description} not a valid object id",),
            }
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_name(&self, name: impl Into<String>, description: &str) -> Result<()> {
        let name = name.into();
        ensure!(
            !name.is_empty(),
            ValidationSnafu {
                message: format!("{description} name cannot be empty"),
            }
        );
        ensure!(
            name.len() <= self.name_max_len as usize,
            ValidationSnafu {
                message: format!(
                    "{description} name too long: {} bytes (max: {})",
                    name.len(),
                    self.name_max_len
                ),
            }
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_markdown(&self, md: &str, description: &str) -> Result<()> {
        ensure!(
            md.len() <= self.markdown_max_len as usize,
            ValidationSnafu {
                message: format!(
                    "{description} markdown too long: {} bytes (max: {})",
                    md.len(),
                    self.markdown_max_len
                ),
            }
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_body(&self, bytes: &bytes::Bytes, description: &str) -> Result<()> {
        ensure!(
            bytes.len() <= self.markdown_max_len as usize,
            ValidationSnafu {
                message: format!(
                    "{description} body too long: {} bytes (max: {})",
                    bytes.len(),
                    self.markdown_max_len
                ),
            }
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_tag(&self, tag: &str, description: &str) -> Result<()> {
        ensure!(
            !tag.is_empty(),
            ValidationSnafu {
                message: format!("{description} tag cannot be an empty string"),
            }
        );
        ensure!(
            tag.len() <= self.tag_max_len as usize,
            ValidationSnafu {
                message: format!(
                    "{description} tag too long: {} bytes (max: {})",
                    tag.len(),
                    self.tag_max_len
                ),
            }
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_num_tags(&self, count: usize, description: &str) -> Result<()> {
        ensure!(
            count <= self.tag_max_count as usize,
            ValidationSnafu {
                message: format!(
                    "{description} too many tags: {count} (max: {})",
                    self.tag_max_count
                ),
            }
        );
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_tags(&self, tags: &[String], description: &str) -> Result<()> {
        self.validate_num_tags(tags.len(), description)?;

        for tag in tags {
            self.validate_tag(tag, description)?;
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn validate_query(&self, query: &[(String, String)]) -> Result<()> {
        let mut query_size = 0;
        for (key, val) in query.iter() {
            query_size += key.len() + val.len() + 1;
        }
        ensure!(
            query_size <= self.max_query_len as usize,
            ValidationSnafu {
                message: format!("query too long {query_size}")
            }
        );
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_validate_name() -> Result<()> {
        let limits = ValidationLimits::default();

        // Valid name
        limits.validate_name("My Object", "")?;

        // Empty is invalid
        assert!(limits.validate_name("", "test").is_err(), "empty name");

        // Too long
        let long = "x".repeat((limits.name_max_len + 1) as usize);
        assert!(
            limits.validate_name(&long, "test").is_err(),
            "too long name"
        );

        Ok(())
    }

    #[test]
    fn test_validate_tag_name() -> Result<()> {
        let limits = ValidationLimits::default();

        // Valid tag
        limits.validate_tag("important", "ok tag")?;

        // Empty is invalid
        assert!(limits.validate_tag("", "test").is_err(), "empty tag");

        // Too long
        let long = "x".repeat((limits.tag_max_len + 1) as usize);
        assert!(limits.validate_tag(&long, "test").is_err(), "tag too long");

        // too many
        assert!(
            limits
                .validate_num_tags((limits.tag_max_count + 1) as usize, "too many")
                .is_err(),
            "too many tags"
        );

        Ok(())
    }

    #[test]
    fn test_validate_id() -> Result<()> {
        let limits = ValidationLimits::default();

        // Valid ID
        let valid_id = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
        limits.validate_id(valid_id, "Object ID")?;

        // Empty is invalid
        assert!(limits.validate_id("", "Object ID").is_err(), "empty oid");

        // Too short
        assert!(
            limits.validate_id("short", "Object ID").is_err(),
            "oid too short"
        );

        // Too long
        let long = "x".repeat((limits.oid_max_len + 1) as usize);
        assert!(
            limits.validate_id(&long, "Object ID").is_err(),
            "oid too long"
        );

        // Control characters
        assert!(
            limits.validate_id("test\x00id", "Object ID").is_err(),
            "oid with invalid chars"
        );

        Ok(())
    }

    #[test]
    fn test_looks_like_object_id() {
        for good_example in [
            "bafyreiafl45wf5eaxiby44pxrkhia3y5jsyix3ov2jzqiftsxjotujqlh4",
            "bafyreifmrdlvfk5uolhph6xmh6geta47auzqjilcsxarpyxlkrbqxks64a",
        ] {
            assert!(looks_like_object_id(good_example));
        }

        for bad_example in [
            "bafyreiafl45wf5eaxiby44pxrkhia3y5jsyix3ov2jzqiftsxjo", // too short
            "bafyreiafl45wf5eaxiby44pxrkhia3y5jsyix3ov2jzqiftsxjotujqlh44", // too long
            "xafyreifmrdlvfk5uolhph6xmh6geta47auzqjilcsxarpyxlkrbqxks64a", // wrong prefix
            "bafyreifmrdlvfk5uolhph6xmh6geta47auzqjilcsxarpyxl0rbqxks64a", // contains '0'
        ] {
            assert!(!looks_like_object_id(bad_example));
        }
    }
}
