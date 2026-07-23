// any-mcp - bounded, workflow-oriented MCP server for Anytype
// SPDX-License-Identifier: Apache-2.0

//! Closed optional-operation ownership shared by library and process tests.

/// Closed inventory of optional production tools and resource families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OptionalOperation {
    OptionalToolsetStatus,
    BodyBlockList,
    BodyBlockCreate,
    BodyBlockUpdate,
    BodyBlockDelete,
    BodyBlockMove,
    RichPageCreate,
    ChatList,
    ChatMessageList,
    ChatMessageGet,
    ChatMessageSearch,
    ChatMessageAdd,
    ChatMessageDelete,
    FileMetadata,
    FileRead,
    FileUpload,
    FileByteResource,
    MemberList,
    MemberGet,
    SpaceCreate,
    SpaceUpdate,
    TypeGet,
    TypeCreate,
    TypeUpdate,
    PropertyCreate,
    PropertyUpdate,
    TagCreate,
    TagUpdate,
    CollectionMemberList,
    CollectionMemberAdd,
    CollectionMemberRemove,
}

impl OptionalOperation {
    /// Every production optional tool and resource-family operation.
    pub const ALL: [Self; 31] = [
        Self::OptionalToolsetStatus,
        Self::BodyBlockList,
        Self::BodyBlockCreate,
        Self::BodyBlockUpdate,
        Self::BodyBlockDelete,
        Self::BodyBlockMove,
        Self::RichPageCreate,
        Self::ChatList,
        Self::ChatMessageList,
        Self::ChatMessageGet,
        Self::ChatMessageSearch,
        Self::ChatMessageAdd,
        Self::ChatMessageDelete,
        Self::FileMetadata,
        Self::FileRead,
        Self::FileUpload,
        Self::FileByteResource,
        Self::MemberList,
        Self::MemberGet,
        Self::SpaceCreate,
        Self::SpaceUpdate,
        Self::TypeGet,
        Self::TypeCreate,
        Self::TypeUpdate,
        Self::PropertyCreate,
        Self::PropertyUpdate,
        Self::TagCreate,
        Self::TagUpdate,
        Self::CollectionMemberList,
        Self::CollectionMemberAdd,
        Self::CollectionMemberRemove,
    ];

    /// Advertised tool name, or `None` for the file byte-resource family.
    pub const fn tool_name(self) -> Option<&'static str> {
        Some(match self {
            Self::OptionalToolsetStatus => "optional_toolset_status",
            Self::BodyBlockList => "body_block_list",
            Self::BodyBlockCreate => "body_block_create",
            Self::BodyBlockUpdate => "body_block_update",
            Self::BodyBlockDelete => "body_block_delete",
            Self::BodyBlockMove => "body_block_move",
            Self::RichPageCreate => "rich_page_create",
            Self::ChatList => "chat_list",
            Self::ChatMessageList => "chat_message_list",
            Self::ChatMessageGet => "chat_message_get",
            Self::ChatMessageSearch => "chat_message_search",
            Self::ChatMessageAdd => "chat_message_add",
            Self::ChatMessageDelete => "chat_message_delete",
            Self::FileMetadata => "file_metadata",
            Self::FileRead => "file_read",
            Self::FileUpload => "file_upload",
            Self::FileByteResource => return None,
            Self::MemberList => "member_list",
            Self::MemberGet => "member_get",
            Self::SpaceCreate => "space_create",
            Self::SpaceUpdate => "space_update",
            Self::TypeGet => "type_get",
            Self::TypeCreate => "type_create",
            Self::TypeUpdate => "type_update",
            Self::PropertyCreate => "property_create",
            Self::PropertyUpdate => "property_update",
            Self::TagCreate => "tag_create",
            Self::TagUpdate => "tag_update",
            Self::CollectionMemberList => "collection_member_list",
            Self::CollectionMemberAdd => "collection_member_add",
            Self::CollectionMemberRemove => "collection_member_remove",
        })
    }

    /// Advertised resource-family template, or `None` for tool operations.
    pub const fn resource_family_name(self) -> Option<&'static str> {
        match self {
            Self::FileByteResource => {
                Some("anytype-file://bytes/{space_id}/{file_id}/{offset}/{length}/{sha256}")
            }
            _ => None,
        }
    }

    /// Production registry whose deterministic workflow owns this operation's fast evidence.
    pub const fn fast_workflow(self) -> OptionalFastWorkflow {
        match self {
            Self::OptionalToolsetStatus => OptionalFastWorkflow::OptionalStatus,
            Self::BodyBlockList
            | Self::BodyBlockCreate
            | Self::BodyBlockUpdate
            | Self::BodyBlockDelete
            | Self::BodyBlockMove
            | Self::RichPageCreate => OptionalFastWorkflow::BodyBlocks,
            Self::ChatList
            | Self::ChatMessageList
            | Self::ChatMessageGet
            | Self::ChatMessageSearch
            | Self::ChatMessageAdd
            | Self::ChatMessageDelete => OptionalFastWorkflow::Chats,
            Self::FileMetadata | Self::FileRead | Self::FileUpload | Self::FileByteResource => {
                OptionalFastWorkflow::Files
            }
            Self::MemberList | Self::MemberGet => OptionalFastWorkflow::Members,
            Self::SpaceCreate
            | Self::SpaceUpdate
            | Self::TypeGet
            | Self::TypeCreate
            | Self::TypeUpdate
            | Self::PropertyCreate
            | Self::PropertyUpdate
            | Self::TagCreate
            | Self::TagUpdate => OptionalFastWorkflow::Schema,
            Self::CollectionMemberList
            | Self::CollectionMemberAdd
            | Self::CollectionMemberRemove => OptionalFastWorkflow::ViewsWrite,
        }
    }

    /// Production registry whose spawned workflow owns this operation's real evidence.
    pub const fn real_workflow(self) -> OptionalRealWorkflow {
        match self {
            Self::OptionalToolsetStatus | Self::MemberList | Self::MemberGet => {
                OptionalRealWorkflow::Members
            }
            Self::BodyBlockList
            | Self::BodyBlockCreate
            | Self::BodyBlockUpdate
            | Self::BodyBlockDelete
            | Self::BodyBlockMove
            | Self::RichPageCreate => OptionalRealWorkflow::BodyBlocks,
            Self::ChatList
            | Self::ChatMessageList
            | Self::ChatMessageGet
            | Self::ChatMessageSearch
            | Self::ChatMessageAdd
            | Self::ChatMessageDelete => OptionalRealWorkflow::Chats,
            Self::FileMetadata | Self::FileRead | Self::FileUpload | Self::FileByteResource => {
                OptionalRealWorkflow::Files
            }
            Self::SpaceCreate
            | Self::SpaceUpdate
            | Self::TypeGet
            | Self::TypeCreate
            | Self::TypeUpdate
            | Self::PropertyCreate
            | Self::PropertyUpdate
            | Self::TagCreate
            | Self::TagUpdate => OptionalRealWorkflow::Schema,
            Self::CollectionMemberList
            | Self::CollectionMemberAdd
            | Self::CollectionMemberRemove => OptionalRealWorkflow::ViewsWrite,
        }
    }
}

/// Closed inventory of deterministic workflows that exercise the production optional surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OptionalFastWorkflow {
    OptionalStatus,
    BodyBlocks,
    Chats,
    Files,
    Members,
    Schema,
    ViewsWrite,
}

impl OptionalFastWorkflow {
    /// The common status workflow followed by all six production registries.
    pub const ALL: [Self; 7] = [
        Self::OptionalStatus,
        Self::BodyBlocks,
        Self::Chats,
        Self::Files,
        Self::Members,
        Self::Schema,
        Self::ViewsWrite,
    ];
}

/// Closed inventory of production registries with executable real-headless workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OptionalRealWorkflow {
    BodyBlocks,
    Chats,
    Files,
    Members,
    Schema,
    ViewsWrite,
}

impl OptionalRealWorkflow {
    /// Every production registry that must run through a spawned headless process.
    pub const ALL: [Self; 6] = [
        Self::BodyBlocks,
        Self::Chats,
        Self::Files,
        Self::Members,
        Self::Schema,
        Self::ViewsWrite,
    ];
}
