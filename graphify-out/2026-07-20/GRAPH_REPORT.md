# Graph Report - anytype-api  (2026-07-19)

## Corpus Check
- 73 files · ~95,609 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 2032 nodes · 5874 edges · 74 communities (60 shown, 14 thin omitted)
- Extraction: 93% EXTRACTED · 7% INFERRED · 0% AMBIGUOUS · INFERRED: 405 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- Chat Mock Server
- File Transfer API
- Space Pagination
- Integration Test Suite
- Authentication API
- Chat Stream Builder
- Filtering and Sorting
- Object Models Utilities
- Pagination Core
- Client Configuration
- Member Integration Tests
- Type Request Models
- Property Setter Tests
- Test Retry Helpers
- Client Cache
- Process Watcher
- Changelog Concepts
- Chat Resolution Client
- Tag API
- Property Value Models
- Chat Message Models
- Member Models
- View Models
- Identifier Resolution
- Property Request Builder
- HTTP Retry Client
- Property Lookup Helpers
- Chat RPC Responses
- Object Creation Builder
- Search API
- Chat Attachments Reactions
- Message Content Formatting
- Input Validation
- Template API
- Type Models
- Cache Controls
- Object Payload Models
- Availability Verification
- Object Accessors
- Chat Example CLI
- Object Update Examples
- HTTP Request Methods
- HTTP Metrics Counters
- HTTP Metrics Reporting
- Chat Query API
- Object Layout Tests
- Tag Request Operations
- Test Result Tracking
- Error Types
- Object CRUD Requests
- Chat Discovery Tests
- Chat Read State
- Object List Pagination
- Example Table Rendering
- Live Smoke Example
- View Objects Example
- Mock Server Binary
- Chat CRUD Tests
- Agenda Example
- Pagination Tests
- Chat Listener Example
- Create Object Example
- File Example
- Filter Expression Example
- Basic Filters Example
- Interactive Auth Example
- List Spaces Example
- List Tasks Example
- Type Property Example
- Pagination Stream Example
- Consistency Retry Example
- Global Search Example
- Space Search Example

## God Nodes (most connected - your core abstractions)
1. `with_test_context_unit()` - 106 edges
2. `with_test_context()` - 99 edges
3. `Filter` - 98 edges
4. `HttpClient` - 87 edges
5. `AnytypeCache` - 79 edges
6. `unique_test_name()` - 68 edges
7. `ValidationLimits` - 67 edges
8. `Object` - 50 edges
9. `AnytypeError` - 43 edges
10. `MockState` - 42 edges

## Surprising Connections (you probably didn't know these)
- `HTTP Retry Middleware` --semantically_similar_to--> `Read-After-Write Verification`  [INFERRED] [semantically similar]
  Troubleshooting.md → README.md
- `test_filter_checkbox_false()` --calls--> `with_test_context()`  [INFERRED]
  tests/test_filters.rs → src/test_util.rs
- `test_filter_checkbox_true()` --calls--> `with_test_context()`  [INFERRED]
  tests/test_filters.rs → src/test_util.rs
- `test_filter_date_after()` --calls--> `with_test_context()`  [INFERRED]
  tests/test_filters.rs → src/test_util.rs
- `test_filter_date_before()` --calls--> `with_test_context()`  [INFERRED]
  tests/test_filters.rs → src/test_util.rs

## Import Cycles
- None detected.

## Hyperedges (group relationships)
- **Authentication and Keystore Flow** — anytype_api_readme_anytype_api_client, anytype_api_keystores_interactive_authentication, anytype_api_keystores_authentication_token_storage, anytype_api_keystores_endpoint_specific_tokens [EXTRACTED 1.00]
- **gRPC Feature Surface** — anytype_api_changelog_grpc_backend, anytype_api_readme_grpc_api_extensions, anytype_api_readme_files_api, anytype_api_readme_chat_streaming, anytype_api_examples_readme_grpc_examples [EXTRACTED 1.00]
- **Client Reliability Mechanisms** — anytype_api_readme_eventual_consistency, anytype_api_readme_read_after_write_verification, anytype_api_troubleshooting_http_retry_middleware [INFERRED 0.85]

## Communities (74 total, 14 thin omitted)

### Community 0 - "Chat Mock Server"
Cohesion: 0.06
Nodes (89): Body, EventValue, Future, JoinHandle, MetadataMap, NamedService, ServerStreamingService, Service (+81 more)

### Community 1 - "File Transfer API"
Cohesion: 0.05
Nodes (58): AnytypeClient, file_from_details(), file_type_filter(), file_type_from_mime(), FileDiscardPreloadRequest, FileDownloadDestination, FileDownloadRequest, FileGetRequest (+50 more)

### Community 2 - "Space Pagination"
Cohesion: 0.06
Nodes (44): ExportFormat, SpaceBackupResult, PagedResult, AnytypeClient, archived_object_from_search_result(), archived_relation_not_found(), archived_search_request(), BackupExportFormat (+36 more)

### Community 3 - "Integration Test Suite"
Cohesion: 0.04
Nodes (87): F, with_test_context_unit(), test_collect_all(), test_create_custom_property(), test_create_multiple_objects(), test_create_with_empty_name(), test_global_search(), test_invalid_object_id() (+79 more)

### Community 4 - "Authentication API"
Cohesion: 0.06
Nodes (42): CredentialStore, AnytypeClient, AuthStatus, CreateApiKeyRequest, CreateApiKeyResponse, CreateChallengeRequest, CreateChallengeResponse, GrpcStatus (+34 more)

### Community 5 - "Chat Stream Builder"
Cohesion: 0.07
Nodes (52): ChatPreview, AnytypeClient, BackoffPolicy, call_subscribe_last_messages(), chat_events_from_event(), chat_events_respect_sub_ids(), ChatEvent, ChatEventStream (+44 more)

### Community 6 - "Filtering and Sorting"
Cohesion: 0.08
Nodes (26): Condition, deserialize_vec_string_or_null(), Filter, FilterExpression, FilterOperator, join_values(), Query, QueryWithFilters (+18 more)

### Community 7 - "Object Models Utilities"
Cohesion: 0.06
Nodes (46): AtomicUsize, Instant, DataModel, ObjectLayout, example_space_id(), AnytypeClient, From, Iter (+38 more)

### Community 8 - "Pagination Core"
Cohesion: 0.06
Nodes (43): BoxStream, Deref, DerefMut, IntoIter, IterMut, &'a mut PaginatedResponse<T>, &'a PagedResult<T>, &'a PaginatedResponse<T> (+35 more)

### Community 9 - "Client Configuration"
Cohesion: 0.08
Nodes (22): AnytypeGrpcConfig, Client, AnytypeClient, ClientConfig, extract_port(), find_grpc(), lsof_listen_ports(), lsof_listen_ports_filters_prefix() (+14 more)

### Community 10 - "Member Integration Tests"
Cohesion: 0.11
Nodes (45): T, with_test_context(), is_expected_member_lookup_error(), String, TestResult, test_active_member_exists(), test_get_member_by_id(), test_get_member_invalid_space() (+37 more)

### Community 11 - "Type Request Models"
Cohesion: 0.13
Nodes (17): AnytypeClient, CreateTypeProperty, CreateTypeRequestBody, ListTypesRequest, NewTypeRequest, Arc, Into, IntoIterator (+9 more)

### Community 12 - "Property Setter Tests"
Cohesion: 0.10
Nodes (42): unique_test_name(), TestResult, test_create_custom_property(), test_create_property_duplicate_key(), test_create_property_invalid_name(), test_delete_property(), test_property_key_stability(), test_read_checkbox_property_value() (+34 more)

### Community 13 - "Test Retry Helpers"
Cohesion: 0.14
Nodes (38): Sleep, create_object_with_retry(), ensure_properties_and_type(), is_key_already_exists_error(), lookup_property_tag_with_retry(), F, TestResult, unique_type_key() (+30 more)

### Community 14 - "Client Cache"
Cohesion: 0.12
Nodes (14): AnytypeCache, Arc, AsRef, Debug, Default, Formatter, HashMap, Option (+6 more)

### Community 15 - "Process Watcher"
Cohesion: 0.14
Nodes (25): matches_process_kind(), next_test_addr(), open_session_events(), ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatcher, ProcessWatcherTimeouts (+17 more)

### Community 16 - "Changelog Concepts"
Cohesion: 0.07
Nodes (38): Ambiguous Resolution Error, Archived Object Management, Changelog, DB Keystore Migration, gRPC Backend, Process Watcher, Resolve Module, Semantic Versioning (+30 more)

### Community 17 - "Chat Resolution Client"
Cohesion: 0.12
Nodes (14): ChatClient<'a>, ChatDeleteMessageRequest, ChatGetMessagesRequest, ChatListRequest, ChatReadAllRequest, ChatReadMessagesRequest, ChatResolveRequest, ChatSearchRequest (+6 more)

### Community 18 - "Tag API"
Cohesion: 0.16
Nodes (14): Color, AnytypeClient, CreateTagRequest, ListTagsRequest, NewTagRequest, Arc, Into, Option (+6 more)

### Community 19 - "Property Value Models"
Cohesion: 0.09
Nodes (15): CreatePropertyRequestBody, deserialize_vec_string_or_null(), deserialize_vec_tag_or_null(), PropertyFormat, PropertyResponse, PropertyValue, PropertyWithValue, D (+7 more)

### Community 20 - "Chat Message Models"
Cohesion: 0.13
Nodes (33): Mark, bool_field(), chat_layout_filter(), ChatMessage, ChatMessagesPage, ChatState, empty_to_none(), filter_id_equal() (+25 more)

### Community 21 - "Member Models"
Cohesion: 0.11
Nodes (17): AnytypeClient, ListMembersRequest, make_member(), Member, MemberRequest, MemberResponse, MemberRole, MemberStatus (+9 more)

### Community 22 - "View Models"
Cohesion: 0.13
Nodes (20): AnytypeClient, deserialize_vec_filter_or_null(), deserialize_vec_sort_or_null(), ListViewsRequest, Arc, D, Error, Into (+12 more)

### Community 23 - "Identifier Resolution"
Cohesion: 0.20
Nodes (17): ambiguous(), AnytypeClient, chat_id_with_space_passes_through(), ChatTarget, not_found(), offline_client(), property_id_passes_through(), Into (+9 more)

### Community 24 - "Property Request Builder"
Cohesion: 0.18
Nodes (9): AnytypeClient, ListPropertiesRequest, NewPropertyRequest, PropertyRequest, Arc, Into, Self, String (+1 more)

### Community 25 - "HTTP Retry Client"
Cohesion: 0.10
Nodes (25): HeaderMap, Method, RequestBuilder, format_bytes(), GetPaged, HttpRequest, is_idempotent_method(), log_and_backoff() (+17 more)

### Community 26 - "Property Lookup Helpers"
Cohesion: 0.17
Nodes (12): K, SP, prime_cache_properties(), Property, AsRef, IntoIterator, Item, Result (+4 more)

### Community 27 - "Chat RPC Responses"
Cohesion: 0.17
Nodes (13): Process, get_messages_after(), Response, subscribe_previews(), grpc_message_content(), Result, ensure_error_ok(), GrpcError (+5 more)

### Community 28 - "Object Creation Builder"
Cohesion: 0.22
Nodes (7): AnytypeClient, NewObjectRequest, Arc, AsRef, Into, Self, String

### Community 29 - "Search API"
Cohesion: 0.15
Nodes (13): AnytypeClient, Arc, Into, IntoIterator, Item, Option, Result, S (+5 more)

### Community 30 - "Chat Attachments Reactions"
Cohesion: 0.14
Nodes (17): chat_details_keys(), ChatAddMessageRequest, ChatEditTextRequest, ChatSendTextRequest, grpc_attachments(), grpc_message_attachment_type(), grpc_message_text_style(), message_attachment_from_grpc() (+9 more)

### Community 31 - "Message Content Formatting"
Cohesion: 0.20
Nodes (5): ChatEditMessageRequest, ChatListMessagesRequest, MessageContent, AsRef, Self

### Community 32 - "Input Validation"
Cohesion: 0.17
Nodes (12): is_base36_chars(), is_cid_chars(), Bytes, Default, Into, Result, Self, String (+4 more)

### Community 33 - "Template API"
Cohesion: 0.19
Nodes (11): AnytypeClient, ListTemplatesRequest, Arc, Into, Option, Result, Self, String (+3 more)

### Community 34 - "Type Models"
Cohesion: 0.15
Nodes (8): deserialize_vec_properties_or_null(), prime_cache_types(), AsRef, D, Error, Result, Type, TypeResponse

### Community 35 - "Cache Controls"
Cohesion: 0.16
Nodes (8): Mutex, Self, sample_property(), sample_space(), sample_type(), test_cache_counts_and_clear(), test_cache_disable_prevents_writes(), test_cache_lookup_property_and_type()

### Community 36 - "Object Payload Models"
Cohesion: 0.15
Nodes (8): Sized, CreateObjectRequestBody, Icon, Value, Vec, UpdateObjectRequest, UpdateObjectRequestBody, SetProperty

### Community 37 - "Availability Verification"
Cohesion: 0.15
Nodes (11): resolve_verify(), Default, Duration, F, Option, Result, Self, T (+3 more)

### Community 38 - "Object Accessors"
Cohesion: 0.27
Nodes (5): Object, DateTime, FixedOffset, Number, Option

### Community 39 - "Chat Example CLI"
Cohesion: 0.27
Nodes (16): Cli, Commands, format_order_id(), hex_to_bytes(), hex_value(), is_hex(), last_five_chars(), list_chats() (+8 more)

### Community 40 - "Object Update Examples"
Cohesion: 0.12
Nodes (13): main(), Result, main(), Result, main(), Result, AnytypeError, Duration (+5 more)

### Community 41 - "HTTP Request Methods"
Cohesion: 0.31
Nodes (7): B, Req, Resp, Arc<HttpClient>, deserialize_json(), Result, T

### Community 42 - "HTTP Metrics Counters"
Cohesion: 0.23
Nodes (3): Form, HttpMetrics, AtomicU64

### Community 43 - "HTTP Metrics Reporting"
Cohesion: 0.18
Nodes (5): HttpClient, HttpMetricsSnapshot, Arc, Display, Formatter

### Community 44 - "Chat Query API"
Cohesion: 0.24
Nodes (7): AnytypeClient, chat_search(), chat_search_space(), ChatClient, ChatGetRequest, ChatListResult, AnytypeClient

### Community 46 - "Tag Request Operations"
Cohesion: 0.36
Nodes (4): refresh_cached_property_tags(), Result, Tag, TagResponse

### Community 47 - "Test Result Tracking"
Cohesion: 0.24
Nodes (4): Self, String, Vec, TestResultTracker

### Community 48 - "Error Types"
Cohesion: 0.25
Nodes (4): AnytypeGrpcError, Error, PathBuf, Self

### Community 49 - "Object CRUD Requests"
Cohesion: 0.29
Nodes (3): object_link_shared(), ObjectRequest, Result

### Community 50 - "Chat Discovery Tests"
Cohesion: 0.54
Nodes (7): AnytypeClient, Result, SocketAddr, setup_client(), test_chat_convenience_reactions_and_read_all(), test_chat_discovery_requests(), wait_for_server()

### Community 51 - "Chat Read State"
Cohesion: 0.33
Nodes (4): ChatReadType, ChatUnreadMessagesRequest, grpc_read_type(), grpc_unread_type()

### Community 52 - "Object List Pagination"
Cohesion: 0.29
Nodes (3): ListObjectsRequest, IntoIterator, Item

### Community 53 - "Example Table Rendering"
Cohesion: 0.60
Nodes (5): format_row(), format_separator(), render_table(), String, Vec

### Community 54 - "Live Smoke Example"
Cohesion: 0.40
Nodes (4): main(), Box, Error, Result

### Community 55 - "View Objects Example"
Cohesion: 0.50
Nodes (4): find_list_object(), main(), Option, Result

### Community 56 - "Mock Server Binary"
Cohesion: 0.40
Nodes (4): main(), Box, Error, Result

### Community 57 - "Chat CRUD Tests"
Cohesion: 0.70
Nodes (4): Result, SocketAddr, test_chat_message_crud(), wait_for_server()

### Community 58 - "Agenda Example"
Cohesion: 0.67
Nodes (3): main(), Result, status_color()

### Community 59 - "Pagination Tests"
Cohesion: 0.67
Nodes (3): TestResult, test_collect_all_matches_total(), test_stream_matches_collect_all()

## Knowledge Gaps
- **13 isolated node(s):** `GrpcError`, `PaginationResponse`, `Semantic Versioning`, `Ambiguous Resolution Error`, `Archived Object Management` (+8 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **14 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Filter` connect `Filtering and Sorting` to `Chat Mock Server`, `File Transfer API`, `Space Pagination`, `Template API`, `Type Request Models`, `Chat Query API`, `Tag API`, `Chat Message Models`, `Member Models`, `Object List Pagination`, `View Models`, `Property Request Builder`, `Property Lookup Helpers`, `Search API`?**
  _High betweenness centrality (0.218) - this node is a cross-community bridge._
- **Why does `HttpClient` connect `HTTP Metrics Reporting` to `Space Pagination`, `Authentication API`, `Pagination Core`, `Client Configuration`, `Type Request Models`, `Tag API`, `Member Models`, `View Models`, `Property Request Builder`, `HTTP Retry Client`, `Property Lookup Helpers`, `Object Creation Builder`, `Search API`, `Input Validation`, `Template API`, `Type Models`, `Object Payload Models`, `HTTP Request Methods`, `HTTP Metrics Counters`, `Tag Request Operations`, `Object CRUD Requests`, `Object List Pagination`?**
  _High betweenness centrality (0.190) - this node is a cross-community bridge._
- **Why does `AnytypeError` connect `Object Update Examples` to `Space Pagination`, `Authentication API`, `Object Models Utilities`, `Pagination Core`, `Member Integration Tests`, `Test Retry Helpers`, `Identifier Resolution`, `Property Lookup Helpers`, `Chat Example CLI`, `HTTP Request Methods`, `Error Types`, `View Objects Example`, `Agenda Example`, `Chat Listener Example`, `Create Object Example`, `Filter Expression Example`, `Basic Filters Example`, `Interactive Auth Example`, `List Spaces Example`, `List Tasks Example`, `Type Property Example`, `Pagination Stream Example`, `Consistency Retry Example`, `Global Search Example`, `Space Search Example`?**
  _High betweenness centrality (0.163) - this node is a cross-community bridge._
- **Are the 102 inferred relationships involving `with_test_context_unit()` (e.g. with `test_collect_all()` and `test_create_custom_property()`) actually correct?**
  _`with_test_context_unit()` has 102 INFERRED edges - model-reasoned connections that need verification._
- **Are the 93 inferred relationships involving `with_test_context()` (e.g. with `test_filter_checkbox_false()` and `test_filter_checkbox_true()`) actually correct?**
  _`with_test_context()` has 93 INFERRED edges - model-reasoned connections that need verification._
- **What connects `GrpcError`, `PaginationResponse`, `Semantic Versioning` to the rest of the system?**
  _13 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Chat Mock Server` be split into smaller, more focused modules?**
  _Cohesion score 0.05536568694463431 - nodes in this community are weakly interconnected._