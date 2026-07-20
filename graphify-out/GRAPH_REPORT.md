# Graph Report - anytype  (2026-07-20)

## Corpus Check
- 157 files · ~371,827 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 5224 nodes · 16141 edges · 159 communities (143 shown, 16 thin omitted)
- Extraction: 97% EXTRACTED · 3% INFERRED · 0% AMBIGUOUS · INFERRED: 527 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `02903fb7`
- Run `git rev-parse HEAD` and compare to check if the graph is stale.
- Run `graphify update .` after code changes (no API cost).

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
- Availability Verification
- Object Accessors
- Object Update Examples
- HTTP Request Methods
- HTTP Metrics Reporting
- Object Layout Tests
- Error Types
- Object CRUD Requests
- Chat Read State
- Object List Pagination
- Example Table Rendering
- Chat CRUD Tests
- Agenda Example
- File Example
- Basic Filters Example
- Interactive Auth Example
- Type Property Example
- Consistency Retry Example
- Space Search Example
- index.rs
- String
- .new
- files.rs
- TestContext
- decode.rs
- Option
- mod.rs
- create_object_with_retry
- NewTagRequest
- unique_test_name
- priority_groups.rs
- chat.rs
- ViewListObjectsRequest
- Vec
- mock.rs
- ProcessWatcher
- Member
- pagination_limit
- main.rs
- String
- MockChatServerHandle
- spaces.rs
- anyr
- ArchiveReader
- ui.rs
- String
- .new
- Self
- auth.rs
- auth.rs
- File
- execute_object_import_batches
- String
- Result
- ListTemplatesRequest
- Result
- .in_space
- Request
- AnytypeGrpcClient
- Text
- view.rs
- TestAnyrCommands
- Changelog
- handle
- AnytypeGrpcError
- views.rs
- chat_messages.rs
- .backup_space
- ensure_list_object
- parse_filters
- Option
- auth.rs
- FileContentResponse
- AnytypeError
- handle
- must_have_body
- Commands
- crypto.rs
- test_chat_stream.rs
- Change
- Changelog
- object_generator.rs
- Changelog
- Setup
- anyback(1)
- common.rs
- Changelog
- FileDownloadRequest
- ImageResizeSchema
- fix_doc_list_indents
- TestResultTracker
- Message
- .in_space
- anyback
- handle
- AnyMcpServer
- ListenSessionEventsSvc
- init-cli-keys.sh
- ListenSessionEventsSvc
- Changelog
- main
- find_list_object
- Anytype gRPC client
- init_tracing
- render_table
- main
- main
- Q: Debug why anytype-api examples fail against the current Anytype server
- anytype-nonet
- raycast-edit-anytype.sh
- test_chat_discovery_requests
- test_collect_all_matches_total
- .listen_session_events
- EmailVerificationStatus
- Anytype Rust Tools and Clients
- prune-templates-keep-oldest.sh

## God Nodes (most connected - your core abstractions)
1. `Result` - 1082 edges
2. `Request` - 416 edges
3. `Response` - 395 edges
4. `ClientCommandsClient<T>` - 343 edges
5. `Status` - 340 edges
6. `with_test_context()` - 107 edges
7. `with_test_context_unit()` - 106 edges
8. `Value` - 100 edges
9. `HttpClient` - 88 edges
10. `AnytypeCache` - 79 edges

## Surprising Connections (you probably didn't know these)
- `execute_object_import_batches()` --calls--> `with_token()`  [INFERRED]
  anyback/src/cli/mod.rs → anytype-rpc/src/auth.rs
- `execute_object_import_path()` --calls--> `with_token()`  [INFERRED]
  anyback/src/cli/mod.rs → anytype-rpc/src/auth.rs
- `handle()` --calls--> `object_link()`  [INFERRED]
  anyr/src/cli/object.rs → anytype-api/src/objects.rs
- `handle()` --calls--> `object_link_shared()`  [INFERRED]
  anyr/src/cli/object.rs → anytype-api/src/objects.rs
- `with_token_request()` --calls--> `with_token()`  [INFERRED]
  anytype-api/src/grpc_util.rs → anytype-rpc/src/auth.rs

## Import Cycles
- None detected.

## Hyperedges (group relationships)
- **Authentication and Keystore Flow** — anytype_api_readme_anytype_api_client, anytype_api_keystores_interactive_authentication, anytype_api_keystores_authentication_token_storage, anytype_api_keystores_endpoint_specific_tokens [EXTRACTED 1.00]
- **gRPC Feature Surface** — anytype_api_changelog_grpc_backend, anytype_api_readme_grpc_api_extensions, anytype_api_readme_files_api, anytype_api_readme_chat_streaming, anytype_api_examples_readme_grpc_examples [EXTRACTED 1.00]

## Communities (159 total, 16 thin omitted)

### Community 0 - "Chat Mock Server"
Cohesion: 0.09
Nodes (16): Color, CreateObjectRequestBody, DataModel, Icon, Object, object_link(), object_link_shared(), ObjectResponse (+8 more)

### Community 1 - "File Transfer API"
Cohesion: 0.01
Nodes (324): AccountSelectTrace, AddFeatured, AddMessage, AddNotificationSubscriber, Ai, AnyNameAllocate, AnyNameIsValid, AnystoreObjectChanges (+316 more)

### Community 2 - "Space Pagination"
Cohesion: 0.15
Nodes (10): App, is_non_markdown_layout(), PathBuf, Terminal, sort_keeps_selected_object_when_possible(), KeyAction, CrosstermBackend, Picker (+2 more)

### Community 3 - "Integration Test Suite"
Cohesion: 0.04
Nodes (7): Block, ClientCommandsClient<T>, Request, Response, Status, View, IntoRequest

### Community 4 - "Authentication API"
Cohesion: 0.07
Nodes (130): add_to_list(), archive_file_paths(), archive_markdown_blob(), archive_object_ids(), archive_payload_file_paths(), assert_non_tty_output_clean(), backup_selected_ids(), ChatMessageTokenCleanupGuard (+122 more)

### Community 5 - "Chat Stream Builder"
Cohesion: 0.17
Nodes (7): GrpcCredentials, HttpCredentials, Into, Option, Self, String, Zeroize

### Community 6 - "Filtering and Sorting"
Cohesion: 0.07
Nodes (27): property_date(), AnytypeClient, CreatePropertyRequestBody, deserialize_vec_string_or_null(), deserialize_vec_tag_or_null(), prime_cache_properties(), Property, PropertyFormat (+19 more)

### Community 7 - "Object Models Utilities"
Cohesion: 0.14
Nodes (21): descriptor_matches_type_filter(), fetch_descriptors_by_ids(), format_since_display(), object_to_descriptor(), parse_local_naive(), parse_local_since(), parse_since(), parse_since_accepts_local_time_without_timezone() (+13 more)

### Community 8 - "Pagination Core"
Cohesion: 0.18
Nodes (17): bool_field(), ChatState, HttpMessageWriteAttachment, HttpMessageWriteBody, HttpMessageWriteMark, last_modified_date(), number_field(), object_from_details() (+9 more)

### Community 9 - "Client Configuration"
Cohesion: 0.07
Nodes (80): ArchiveObjectInfo, build_archive_object_index(), convert_archive_object_pb_to_markdown(), convert_archive_object_to_markdown(), convert_archive_snapshot_to_markdown(), convert_pb_json_snapshot_to_markdown(), convert_pb_snapshot_to_markdown(), convert_sample_pb_json_object_to_markdown_contains_headings() (+72 more)

### Community 10 - "Member Integration Tests"
Cohesion: 0.25
Nodes (11): archive_basename(), ArchiveCmpChanged, ArchiveCmpObject, ArchiveCmpReport, build_archive_cmp_report(), cmp_value_to_text(), collect_cmp_objects(), DiffArgs (+3 more)

### Community 11 - "Type Request Models"
Cohesion: 0.27
Nodes (3): AnytypeClient, F, Into

### Community 12 - "Property Setter Tests"
Cohesion: 0.10
Nodes (39): build_yaml_front_matter(), CachedObject, clamp_scroll(), detail_string(), detail_value_to_string(), epoch_to_rfc3339(), extract_space_id_from_archive_name(), fixture_app() (+31 more)

### Community 13 - "Test Retry Helpers"
Cohesion: 0.11
Nodes (15): AnytypeClient, ListObjectsRequest, NewObjectRequest, ObjectRequest, Arc, AsRef, Filter, Into (+7 more)

### Community 14 - "Client Cache"
Cohesion: 0.04
Nodes (110): Align, Align, Amend, Auth, AutoArchive, AutoRestore, Avatar, BackgroundColor (+102 more)

### Community 15 - "Process Watcher"
Cohesion: 0.25
Nodes (3): HttpMetricsSnapshot, Display, Formatter

### Community 16 - "Changelog Concepts"
Cohesion: 0.25
Nodes (4): file_type_from_mime(), FilePreloadRequest, FileType, grpc_file_type()

### Community 17 - "Chat Resolution Client"
Cohesion: 0.25
Nodes (5): fmt_masked(), KeyStoreType, Display, Formatter, Debug

### Community 18 - "Tag API"
Cohesion: 0.07
Nodes (47): AnytypeClient, BackoffPolicy, call_subscribe_last_messages(), chat_events_from_event(), chat_events_respect_sub_ids(), ChatEvent, ChatEventStream, ChatStreamBuilder (+39 more)

### Community 19 - "Property Value Models"
Cohesion: 0.22
Nodes (7): Checkbox, Date, Detail, Placeholder, Status, Tag, Value

### Community 20 - "Chat Message Models"
Cohesion: 0.29
Nodes (6): any-mcp, Build, License, Phase 1 scaffold, Protocol channel, Source layout

### Community 22 - "View Models"
Cohesion: 0.33
Nodes (5): ChatService, Poll, Body, NamedService, Service

### Community 23 - "Identifier Resolution"
Cohesion: 0.04
Nodes (87): F, with_test_context_unit(), test_collect_all(), test_create_custom_property(), test_create_multiple_objects(), test_create_with_empty_name(), test_global_search(), test_invalid_object_id() (+79 more)

### Community 24 - "Property Request Builder"
Cohesion: 0.50
Nodes (3): Added, Changelog, [Unreleased]

### Community 25 - "HTTP Retry Client"
Cohesion: 0.04
Nodes (64): assert_backup_args_equal(), AuthArgs, AuthCommands, backup_export_options(), backup_export_options_maps_include_flags_and_pb_json(), backup_export_options_maps_markdown_include_properties(), backup_target_always_uses_zip_extension_for_generated_name(), backup_target_dest_must_not_exist() (+56 more)

### Community 26 - "Property Lookup Helpers"
Cohesion: 0.09
Nodes (84): add_to_list(), alpha_suffix(), backup_manifest_object(), backup_selected(), backup_selected_ids(), choose_two_distinct_writable_spaces_cli(), clone_sqlite_with_sidecars(), cloned_test_keystore() (+76 more)

### Community 27 - "Chat RPC Responses"
Cohesion: 0.07
Nodes (50): parse_message_mark(), parse_message_marks(), chat_layout_filter(), ChatEditTextRequest, current_http_message_schema_preserves_available_fields(), empty_to_none(), filter_id_equal(), filter_name_equal() (+42 more)

### Community 28 - "Object Creation Builder"
Cohesion: 0.50
Nodes (4): Limit, ViewId, Widget, Layout

### Community 29 - "Search API"
Cohesion: 0.03
Nodes (32): AutofillMode, Code, Context, DetailsSet, DeviceAdd, DeviceState, GenericErrorResponse, Language (+24 more)

### Community 31 - "Message Content Formatting"
Cohesion: 0.07
Nodes (27): AnytypeClient, CreateTypeProperty, CreateTypeRequestBody, deserialize_vec_properties_or_null(), ListTypesRequest, NewTypeRequest, prime_cache_types(), Arc (+19 more)

### Community 33 - "Template API"
Cohesion: 0.05
Nodes (38): Condition, deserialize_vec_string_or_null(), Filter, FilterExpression, FilterOperator, join_values(), Query, QueryWithFilters (+30 more)

### Community 35 - "Cache Controls"
Cohesion: 0.07
Nodes (36): Arc<HttpClient>, deserialize_json(), format_bytes(), GetPaged, HttpClient, HttpMetrics, HttpRequest, is_idempotent_method() (+28 more)

### Community 37 - "Availability Verification"
Cohesion: 0.05
Nodes (76): Account, AppInfo, Attachment, Auth, Bookmark, Chat, ChatMessage, ChatState (+68 more)

### Community 40 - "Object Update Examples"
Cohesion: 0.10
Nodes (63): archive_file_listing(), archive_signature(), AttachmentCaseBatch, BatchArtifacts, choose_writable_chat_space(), choose_writable_spaces(), cleanup_by_prefix(), cleanup_source_ids() (+55 more)

### Community 43 - "HTTP Metrics Reporting"
Cohesion: 0.06
Nodes (42): &'a mut PaginatedResponse<T>, &'a PagedResult<T>, &'a PaginatedResponse<T>, create_test_request(), next_response_iter(), PagedResult, PagedResult<T>, PaginatedResponse (+34 more)

### Community 45 - "Object Layout Tests"
Cohesion: 0.07
Nodes (49): Account, Add, BlockField, Cafe, ChatPreview, Config, Details, Device (+41 more)

### Community 48 - "Error Types"
Cohesion: 0.04
Nodes (9): Path, run_inspector(), init_tracing(), main(), run(), main(), Status, Result (+1 more)

### Community 49 - "Object CRUD Requests"
Cohesion: 0.03
Nodes (27): Block, BlockMetaOnly, Condition, Config, DataviewRestriction, Description, EmptyType, FormulaType (+19 more)

### Community 51 - "Chat Read State"
Cohesion: 0.15
Nodes (17): KeyStoreError, From, PathBuf, String, VarError, default_platform_keyring(), init_keystore(), KeyStore (+9 more)

### Community 52 - "Object List Pagination"
Cohesion: 0.09
Nodes (46): AuthArgs, AuthCommand, AuthSource, AuthStatusArgs, build_yaml_export(), ConfigFile, detect_scope(), ExportHeaderFormat (+38 more)

### Community 53 - "Example Table Rendering"
Cohesion: 0.03
Nodes (21): ActionType, Align, CardStyle, Code, DateFormat, EmailVerificationStatus, FileIndexingStatus, Format (+13 more)

### Community 57 - "Chat CRUD Tests"
Cohesion: 0.05
Nodes (38): AddChatMessageResponse, AnytypeClient, chat_details_keys(), chat_search(), chat_search_space(), ChatClient<'a>, ChatCreateRequest, ChatDeleteMessageRequest (+30 more)

### Community 58 - "Agenda Example"
Cohesion: 0.09
Nodes (52): backup_selected_ids(), BugDisposition, CaseStatus, choose_two_distinct_writable_spaces_cli(), choose_writable_space_cli(), clone_sqlite_with_sidecars(), cloned_test_keystore(), configure_test_keystore() (+44 more)

### Community 62 - "File Example"
Cohesion: 0.15
Nodes (19): ambiguous(), AnytypeClient, chat_id_with_space_passes_through(), ChatTarget, not_found(), offline_client(), property_id_passes_through(), Into (+11 more)

### Community 64 - "Basic Filters Example"
Cohesion: 0.10
Nodes (20): AnytypeCache, Arc, AsRef, Default, Formatter, HashMap, Mutex, Option (+12 more)

### Community 65 - "Interactive Auth Example"
Cohesion: 0.08
Nodes (19): AnytypeClient, ClientConfig, extract_port(), find_grpc(), lsof_listen_ports(), lsof_listen_ports_filters_prefix(), probe_grpc_port(), AnytypeGrpcClient (+11 more)

### Community 68 - "Type Property Example"
Cohesion: 0.13
Nodes (30): apply_import_response(), BackupSelection, build_import_plan(), build_import_plan_infers_ids_without_manifest_from_directory(), build_import_plan_uses_archive_path_directly(), collect_import_snapshots(), descriptors_from_selection(), format_import_api_error() (+22 more)

### Community 70 - "Consistency Retry Example"
Cohesion: 0.08
Nodes (18): chat_message_path(), ChatAddMessageRequest, ChatEditMessageRequest, ChatHttpAddMessageRequest, ChatHttpEditMessageRequest, ChatHttpListRequest, ChatSendTextRequest, MessageAttachment (+10 more)

### Community 72 - "Space Search Example"
Cohesion: 0.10
Nodes (50): T, unique_suffix(), with_test_context(), TestResult, test_chat_message_crud(), test_rest_chat_message_crud(), is_expected_member_lookup_error(), String (+42 more)

### Community 74 - "index.rs"
Cohesion: 0.13
Nodes (38): ArchiveIndex, build_preview(), build_preview_is_stable_and_compact(), build_preview_preserves_markdown_lines_without_truncating_headings(), collect_link_candidates(), collect_preview_strings(), collect_user_properties(), collect_user_properties_includes_array_and_object_values() (+30 more)

### Community 76 - "String"
Cohesion: 0.12
Nodes (14): AnytypeClient, conditional_not_modified_status_is_preserved(), FileContentRequest, FileDeleteRequest, FileDiscardPreloadRequest, FileGetRequest, FilesClient<'a>, head_returns_file_metadata_without_a_body() (+6 more)

### Community 77 - ".new"
Cohesion: 0.17
Nodes (11): ChatClient, ChatHttpMessageStreamRequest, grpc_message_conversion_retains_rich_state(), mock_http_client(), rest_add_message_sends_current_wire_shape(), rest_chat_stream_rejects_invalid_configuration_before_connecting(), rest_chat_stream_sends_configuration_and_decodes_typed_events(), rest_edit_message_uses_patch_and_http_style_names() (+3 more)

### Community 79 - "files.rs"
Cohesion: 0.11
Nodes (32): file_from_details(), file_from_http_upload(), FileObject, FilesClient, FileStyle, filter_id_equal(), filter_to_dataview(), grpc_file_style() (+24 more)

### Community 80 - "TestContext"
Cohesion: 0.07
Nodes (32): example_space_id(), AnytypeClient, From, Iter, Mutex, PathBuf, Self, String (+24 more)

### Community 82 - "decode.rs"
Cohesion: 0.14
Nodes (41): build_expanded_entry_from_details(), derive_layout_name(), detail_value(), ExpandedSnapshotEntry, format_datetime_display(), format_datetime_with_tz(), format_last_modified(), format_utc_datetime_with_tz() (+33 more)

### Community 83 - "Option"
Cohesion: 0.05
Nodes (25): Amount, AttachmentType, Cart, CartProduct, CryptoCheckout, Data, DataSource, DeviceNetworkType (+17 more)

### Community 85 - "create_object_with_retry"
Cohesion: 0.13
Nodes (40): create_object_with_retry(), ensure_properties_and_type(), is_key_already_exists_error(), lookup_property_tag_with_retry(), F, Object, Tag, TestResult (+32 more)

### Community 88 - "NewTagRequest"
Cohesion: 0.08
Nodes (27): AnytypeClient, CreateTagRequest, ListTagsRequest, NewTagRequest, refresh_cached_property_tags(), Arc, Color, Filter (+19 more)

### Community 89 - "unique_test_name"
Cohesion: 0.10
Nodes (42): unique_test_name(), TestResult, test_create_custom_property(), test_create_property_duplicate_key(), test_create_property_invalid_name(), test_delete_property(), test_property_key_stability(), test_read_checkbox_property_value() (+34 more)

### Community 95 - "priority_groups.rs"
Cohesion: 0.09
Nodes (40): assert_case_registered(), case_replace_after_object_type_changed_since_backup(), case_replace_object_type_collection_with_items(), case_replace_object_type_complex_nested_object(), case_replace_object_type_custom_type_object(), case_replace_object_type_file(), case_replace_object_type_object(), case_replace_object_type_property() (+32 more)

### Community 98 - "chat.rs"
Cohesion: 0.16
Nodes (25): decode_order_id_arg(), decode_order_id_hex_roundtrip(), decode_order_id_non_hex_passthrough(), encode_order_id_hex(), handle(), hex_char(), hex_value(), is_hex_string() (+17 more)

### Community 99 - "ViewListObjectsRequest"
Cohesion: 0.13
Nodes (20): AnytypeClient, deserialize_vec_filter_or_null(), deserialize_vec_sort_or_null(), ListViewsRequest, Arc, D, Filter, Into (+12 more)

### Community 100 - "Vec"
Cohesion: 0.09
Nodes (31): BlockParticipant, ChangePayload, DataviewRestrictions, DetailsSet, FileEncryptionKey, FileInfo, HistorySize, IdentityProfile (+23 more)

### Community 102 - "mock.rs"
Cohesion: 0.11
Nodes (35): chat_add_value(), chat_delete_value(), chat_details(), chat_state_update_value(), chat_update_value(), ChatRoom, delete_message_error(), edit_message_error() (+27 more)

### Community 103 - "ProcessWatcher"
Cohesion: 0.14
Nodes (23): matches_process_kind(), next_test_addr(), open_session_events(), ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatcher, ProcessWatcherTimeouts (+15 more)

### Community 107 - "Member"
Cohesion: 0.10
Nodes (17): AnytypeClient, ListMembersRequest, make_member(), Member, MemberRequest, MemberResponse, MemberRole, MemberStatus (+9 more)

### Community 110 - "pagination_limit"
Cohesion: 0.10
Nodes (18): handle(), AppContext, ListArgs, pagination_limit(), pagination_offset(), PropertyArgs, TagArgs, handle() (+10 more)

### Community 113 - "main.rs"
Cohesion: 0.16
Nodes (29): init_logging(), auth_login(), auth_logout(), AuthCommand, check_auth_status(), Cli, Commands, copy_link_command() (+21 more)

### Community 114 - "String"
Cohesion: 0.18
Nodes (33): ChatReadTypeArg, AuthCommands, ChatCommands, ChatMessagesArgs, ChatMessagesCommands, FileCommands, FilterArgs, ListArgs (+25 more)

### Community 115 - "MockChatServerHandle"
Cohesion: 0.17
Nodes (7): MockChatServer, MockChatServerHandle, Default, JoinHandle, Receiver, Self, SocketAddr

### Community 116 - "spaces.rs"
Cohesion: 0.05
Nodes (44): AnytypeClient, archived_object_from_search_result(), archived_relation_not_found(), archived_search_request(), BackupExportFormat, BackupSpaceRequest, CreateSpaceRequestBody, dataview_filter_checkbox_equal() (+36 more)

### Community 121 - "anyr"
Cohesion: 0.06
Nodes (32): Accessibility Permissions, any-edit: Edit Anytype document in external editor, Build from source, Commands, Configure, Install, License, Platform compatibility (+24 more)

### Community 123 - "ArchiveReader"
Cohesion: 0.12
Nodes (21): ArchiveFileEntry, ArchiveReader, ArchiveSourceKind, infer_object_id_from_snapshot_path(), infer_object_ids_from_files(), looks_like_content_id(), reader_lists_and_reads_directory_archive(), reader_lists_and_reads_zip_archive() (+13 more)

### Community 124 - "ui.rs"
Cohesion: 0.15
Nodes (33): buffer_to_lines(), draw(), draw_contents_panel(), draw_footer(), draw_help_overlay(), draw_links_panel(), draw_main_area(), draw_metadata_panel() (+25 more)

### Community 125 - "String"
Cohesion: 0.11
Nodes (17): column_widths(), FileObject, format_row(), format_separator(), Member, Object, Property, render_table() (+9 more)

### Community 126 - ".new"
Cohesion: 0.21
Nodes (7): authenticate(), broadcast_event(), build_chat_state(), build_event(), Status, Future, MetadataMap

### Community 131 - "Self"
Cohesion: 0.13
Nodes (9): file_type_filter(), FileListRequest, FileSearchRequest, filter_not_empty(), Filter, IntoIterator, Self, Vec (+1 more)

### Community 134 - "auth.rs"
Cohesion: 0.26
Nodes (11): AuthStatus, CreateApiKeyRequest, CreateApiKeyResponse, CreateChallengeRequest, CreateChallengeResponse, GrpcStatus, HttpStatus, KeyStoreStatus (+3 more)

### Community 137 - "auth.rs"
Cohesion: 0.18
Nodes (20): create_local_link_challenge(), create_session(), create_session_token(), create_session_token_from_account_key(), create_session_token_from_app_key(), LocalLinkCredentials, AsRef, Channel (+12 more)

### Community 138 - "File"
Cohesion: 0.10
Nodes (17): Bookmark, Description, FaviconHash, File, Hash, ImageHash, Mime, Name (+9 more)

### Community 143 - "execute_object_import_batches"
Cohesion: 0.09
Nodes (39): aggregate_import_responses(), AppContext, build_import_plan_infers_ids_without_manifest_from_zip(), dir_contains_pb_or_json(), execute_object_import(), execute_object_import_batches(), execute_object_import_path(), finalize_backup_output_path() (+31 more)

### Community 144 - "String"
Cohesion: 0.13
Nodes (16): Dataview, Filter, Group, ObjectType, RelationLink, Block, FileInfo, Filter (+8 more)

### Community 146 - "Result"
Cohesion: 0.08
Nodes (22): ListPropertiesRequest, NewPropertyRequest, PropertyRequest, Arc, Color, Filter, Into, IntoIterator (+14 more)

### Community 147 - "ListTemplatesRequest"
Cohesion: 0.19
Nodes (12): AnytypeClient, ListTemplatesRequest, Arc, Filter, Into, Object, Option, Self (+4 more)

### Community 148 - "Result"
Cohesion: 0.20
Nodes (32): exit_code(), main(), Box, T, with_token(), auth_status(), connect(), disable_sharing() (+24 more)

### Community 155 - ".in_space"
Cohesion: 0.08
Nodes (36): ChatHttpEvent, ChatHttpEventStream, ChatHttpSseState, ChatMessage, ChatMessageSearchPage, ChatMessageSearchResult, ChatMessagesPage, filter_unread_messages() (+28 more)

### Community 156 - "Request"
Cohesion: 0.33
Nodes (21): ChatAddMessageSvc, ChatDeleteMessageSvc, ChatEditMessageSvc, ChatGetMessagesByIdsSvc, ChatGetMessagesSvc, ChatReadAllSvc, ChatReadMessagesSvc, ChatSubscribeLastMessagesSvc (+13 more)

### Community 157 - "AnytypeGrpcClient"
Cohesion: 0.19
Nodes (10): AnytypeGrpcClient, AnytypeGrpcConfig, default_grpc_endpoint(), AsRef, Channel, Default, Into, Self (+2 more)

### Community 158 - "Text"
Cohesion: 0.11
Nodes (18): Checked, Color, Div, IconEmoji, IconImage, Latex, Link, Marks (+10 more)

### Community 164 - "view.rs"
Cohesion: 0.31
Nodes (19): columns_for_items(), default_columns(), handle(), load_property_names(), object_value_for_relation(), override_columns(), AppContext, BTreeMap (+11 more)

### Community 165 - "TestAnyrCommands"
Cohesion: 0.17
Nodes (7): anyr_bin(), base_env(), run_anyr(), run_anyr_json(), run_help(), TestAnyrCommands, CompletedProcess

### Community 170 - "Changelog"
Cohesion: 0.09
Nodes (21): [0.2.2] - anyr - 2026-01-12, [0.2.3] - anyr - 2026-01-12, [0.2.4] - anyr - 2026-01-17, [0.3.0] - anyr - 2026-01-28, [0.4.0] - anyr - 2026-02-16, [0.4.1], Added, Added (+13 more)

### Community 171 - "handle"
Cohesion: 0.18
Nodes (17): apply_file_filters_list(), apply_file_filters_search(), download_http(), FileType, handle(), merge_properties(), parse_properties(), AppContext (+9 more)

### Community 172 - "AnytypeGrpcError"
Cohesion: 0.19
Nodes (18): AnytypeHeadlessConfig, default_headless_config_path(), load_headless_config(), Option, Path, PathBuf, String, AnytypeGrpcError (+10 more)

### Community 173 - "views.rs"
Cohesion: 0.13
Nodes (23): BlockDataview, ClientCommandsClient, RelationFormat, F, T, fetch_grid_view_columns(), find_dataview_block(), GridViewColumn (+15 more)

### Community 179 - "chat_messages.rs"
Cohesion: 0.05
Nodes (36): main(), Cli, Commands, format_order_id(), hex_to_bytes(), hex_value(), is_hex(), last_five_chars() (+28 more)

### Community 180 - ".backup_space"
Cohesion: 0.22
Nodes (12): AnytypeGrpcClient, generated_target_name(), ExportFormat, Into, PathBuf, Self, String, Vec (+4 more)

### Community 183 - "ensure_list_object"
Cohesion: 0.34
Nodes (15): ObjectLayout, ensure_list_object(), find_list_object_by_layout(), list_views_with_retry(), Object, Option, TestResult, Vec (+7 more)

### Community 185 - "parse_filters"
Cohesion: 0.20
Nodes (16): handle(), AppContext, MemberArgs, parse_bool(), parse_condition(), parse_filter(), parse_filters(), parse_number() (+8 more)

### Community 186 - "Option"
Cohesion: 0.16
Nodes (10): FileDownloadDestination, FileHttpUploadRequest, FileSource, FileUploadRequest, FileUploadResponse, http_upload_file(), Bytes, Option (+2 more)

### Community 189 - "auth.rs"
Cohesion: 0.29
Nodes (12): handle(), HeadlessConfig, login(), logout(), AppContext, AuthArgs, Option, PathBuf (+4 more)

### Community 190 - "FileContentResponse"
Cohesion: 0.19
Nodes (10): file_http_metadata(), file_path(), FileContentResponse, FileHttpMetadata, header_string(), insert_optional_header(), HeaderMap, Method (+2 more)

### Community 200 - "handle"
Cohesion: 0.28
Nodes (14): handle(), HeadlessConfig, login(), logout(), AppContext, AuthArgs, Option, PathBuf (+6 more)

### Community 201 - "must_have_body"
Cohesion: 0.36
Nodes (8): AppContext, build_client(), Cli, resolve_output_format(), resolve_table_date_format(), AnytypeClient, Commands, run()

### Community 202 - "Commands"
Cohesion: 0.15
Nodes (12): ChatArgs, Commands, AuthArgs, Box, ListArgs, ObjectArgs, SpaceArgs, TemplateArgs (+4 more)

### Community 203 - "crypto.rs"
Cohesion: 0.32
Nodes (11): crc16_xmodem(), derive_keys_from_mnemonic(), derive_keys_from_mnemonic_go_vector(), encode_account_id(), String, slip10_derive_child(), slip10_derive_master(), slip10_derive_path() (+3 more)

### Community 206 - "test_chat_stream.rs"
Cohesion: 0.27
Nodes (10): chat_stream_receives_messages(), chat_stream_reconnects_after_disconnect(), rest_chat_stream_receives_initial_message(), AnytypeClient, F, SocketAddr, setup_mock_client(), wait_for_event() (+2 more)

### Community 207 - "Change"
Cohesion: 0.17
Nodes (13): Change, ChangeNoSnapshot, Content, DocumentCreate, DocumentDelete, FileKeys, HashMap, Snapshot (+5 more)

### Community 216 - "Changelog"
Cohesion: 0.15
Nodes (12): [0.1.1] - any-edit, [0.1.2] - any-edit - 2026-01-17, [0.1.3] - any-edit - 2026-01-28, [0.1.5], Added, Changed, Changed, Changed (+4 more)

### Community 217 - "object_generator.rs"
Cohesion: 0.35
Nodes (11): cleanup_by_ids(), cleanup_by_name_prefix(), create_object_once(), create_object_with_retry(), generate_fixture(), GeneratedFixture, GeneratedObject, AnytypeClient (+3 more)

### Community 218 - "Changelog"
Cohesion: 0.15
Nodes (12): [0.2.0] - anytype-rpc - 2026-01-17, [0.2.1] - anytype-rpc - 2026-01-28, [0.3.0] - anytype-rpc - 2026-02-16, [0.3.1], Added, Added, Added, Changed (+4 more)

### Community 222 - "Setup"
Cohesion: 0.17
Nodes (11): 1) Authenticate, 2) Configure the script, 3) Add the script to Raycast, 4) Assign a hotkey, 5) Grant Accessibility permissions (macOS), Common issues, Diagnostics, Raycast setup and diagnostics (+3 more)

### Community 223 - "anyback(1)"
Cohesion: 0.17
Nodes (11): anyback(1), BACKUP OUTPUT, DESCRIPTION, ENVIRONMENT VARIABLES, EXIT STATUS, EXTRACT, GLOBAL OPTIONS, NAME (+3 more)

### Community 224 - "common.rs"
Cohesion: 0.25
Nodes (14): emit_message_rows(), format_sender(), ChatMessage, Option, build_member_identity_map(), load_member_cache(), MemberCache, parse_member_identity() (+6 more)

### Community 225 - "Changelog"
Cohesion: 0.06
Nodes (41): Ambiguous Resolution Error, Archived Object Management, Changelog, DB Keystore Migration, gRPC Backend, Process Watcher, Resolve Module, Semantic Versioning (+33 more)

### Community 227 - "FileDownloadRequest"
Cohesion: 0.31
Nodes (5): FileDownloadRequest, rich_and_url_uploads_select_grpc(), AsRef, Path, simple_path_and_byte_uploads_select_rest()

### Community 228 - "ImageResizeSchema"
Cohesion: 0.35
Nodes (11): FileInfo, FileKeys, ImageResizeSchema, Link, HashMap, Link, Option, String (+3 more)

### Community 235 - "fix_doc_list_indents"
Cohesion: 0.42
Nodes (8): fix_doc_list_indents(), indent_doc_list_continuation(), indent_doc_list_line(), main(), Box, Option, PathBuf, String

### Community 239 - "TestResultTracker"
Cohesion: 0.24
Nodes (4): Self, String, Vec, TestResultTracker

### Community 241 - "Message"
Cohesion: 0.22
Nodes (9): BlockUpdate, DropFiles, Export, Import, Message, Migration, PreloadFile, Value (+1 more)

### Community 245 - ".in_space"
Cohesion: 0.40
Nodes (5): Cli, Command, AuthArgs, ObjectArgs, SpaceArgs

### Community 249 - "anyback"
Cohesion: 0.22
Nodes (8): anyback, Commands, Development, Features, Integrity Testing, Library Crate, Restore Transport, Usage Notes

### Community 250 - "handle"
Cohesion: 0.23
Nodes (11): must_have_body(), AsRef, Into, Path, handle(), merge_properties(), parse_properties(), AppContext (+3 more)

### Community 251 - "AnyMcpServer"
Cohesion: 0.36
Nodes (5): advertises_upcoming_protocol_revision_and_server_identity(), AnyMcpServer, Self, ServerHandler, ServerInfo

### Community 256 - "ListenSessionEventsSvc"
Cohesion: 0.15
Nodes (13): Account, EventListener, EventStream, ListenSessionEventsSvc, AtomicU64, Box, Pin, Sender (+5 more)

### Community 257 - "init-cli-keys.sh"
Cohesion: 0.32
Nodes (5): ANYTYPE_GRPC_ENDPOINT, ANYTYPE_URL, init_cli_and_keystore(), join_space(), init-cli-keys.sh script

### Community 260 - "Changelog"
Cohesion: 0.29
Nodes (6): 0.1.0 - 2026-02-10, [0.3.0 - alpha] - anyback - 2026-02-16, [0.4.0-alpha.2], Changed, Changelog, [Unreleased]

### Community 261 - "main"
Cohesion: 0.50
Nodes (4): main(), MessageContent, Object, status_color()

### Community 262 - "find_list_object"
Cohesion: 0.50
Nodes (4): find_list_object(), main(), Object, Option

### Community 263 - "Anytype gRPC client"
Cohesion: 0.29
Nodes (6): Anytype gRPC client, Building, Compatibility, License, Related projects, Status and plan

### Community 271 - "init_tracing"
Cohesion: 0.60
Nodes (4): ColorArg, init_tracing(), main(), run()

### Community 272 - "render_table"
Cohesion: 0.60
Nodes (5): format_row(), format_separator(), render_table(), String, Vec

### Community 287 - "Q: Debug why anytype-api examples fail against the current Anytype server"
Cohesion: 0.40
Nodes (4): Answer, Outcome, Q: Debug why anytype-api examples fail against the current Anytype server, Source Nodes

### Community 295 - "raycast-edit-anytype.sh"
Cohesion: 0.67
Nodes (3): EDITOR_COMMAND, notify(), raycast-edit-anytype.sh script

### Community 312 - "test_chat_discovery_requests"
Cohesion: 0.67
Nodes (3): TestResult, test_chat_discovery_requests(), test_rest_chat_messages_reactions_search_and_reads()

### Community 314 - "test_collect_all_matches_total"
Cohesion: 0.67
Nodes (3): TestResult, test_collect_all_matches_total(), test_stream_matches_collect_all()

## Knowledge Gaps
- **443 isolated node(s):** `EDITOR_COMMAND`, `AuthCommand`, `ResultOutput`, `go-testvec`, `GrpcError` (+438 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **16 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Result` connect `Error Types` to `Chat Mock Server`, `Space Pagination`, `Integration Test Suite`, `Authentication API`, `Chat Stream Builder`, `Filtering and Sorting`, `Object Models Utilities`, `Pagination Core`, `Client Configuration`, `Member Integration Tests`, `Type Request Models`, `Property Setter Tests`, `Test Retry Helpers`, `Process Watcher`, `Changelog Concepts`, `Chat Resolution Client`, `Tag API`, `View Models`, `HTTP Retry Client`, `Property Lookup Helpers`, `Chat RPC Responses`, `Message Content Formatting`, `Template API`, `Cache Controls`, `Availability Verification`, `Object Update Examples`, `HTTP Metrics Reporting`, `Object CRUD Requests`, `Chat Read State`, `Object List Pagination`, `Chat CRUD Tests`, `Agenda Example`, `File Example`, `Basic Filters Example`, `Interactive Auth Example`, `Type Property Example`, `Consistency Retry Example`, `index.rs`, `String`, `.new`, `files.rs`, `TestContext`, `decode.rs`, `NewTagRequest`, `chat.rs`, `ViewListObjectsRequest`, `Vec`, `ProcessWatcher`, `Member`, `pagination_limit`, `main.rs`, `String`, `MockChatServerHandle`, `spaces.rs`, `ArchiveReader`, `.new`, `auth.rs`, `execute_object_import_batches`, `Result`, `ListTemplatesRequest`, `Result`, `.in_space`, `AnytypeGrpcClient`, `view.rs`, `handle`, `AnytypeGrpcError`, `views.rs`, `chat_messages.rs`, `.backup_space`, `parse_filters`, `Option`, `auth.rs`, `FileContentResponse`, `handle`, `must_have_body`, `Commands`, `crypto.rs`, `test_chat_stream.rs`, `object_generator.rs`, `common.rs`, `fix_doc_list_indents`, `handle`, `ListenSessionEventsSvc`, `main`, `find_list_object`, `init_tracing`, `main`, `main`, `.listen_session_events`?**
  _High betweenness centrality (0.609) - this node is a cross-community bridge._
- **Why does `Request` connect `Integration Test Suite` to `ListenSessionEventsSvc`, `File Transfer API`, `File`, `Client Cache`, `Tag API`, `Property Value Models`, `Result`, `View Models`, `Request`, `AnytypeGrpcClient`, `Text`, `Search API`, `Availability Verification`, `Object Layout Tests`, `Error Types`, `Change`, `mock.rs`, `spaces.rs`, `.new`?**
  _High betweenness centrality (0.043) - this node is a cross-community bridge._
- **Why does `Response` connect `Integration Test Suite` to `File Transfer API`, `Client Cache`, `execute_object_import_batches`, `String`, `Tag API`, `Property Value Models`, `Result`, `Search API`, `Template API`, `Cache Controls`, `Availability Verification`, `Object Layout Tests`, `Error Types`, `Type Property Example`, `Change`, `Option`, `.listen_session_events`, `Vec`, `Message`, `spaces.rs`?**
  _High betweenness centrality (0.040) - this node is a cross-community bridge._
- **What connects `EDITOR_COMMAND`, `AuthCommand`, `ResultOutput` to the rest of the system?**
  _443 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Chat Mock Server` be split into smaller, more focused modules?**
  _Cohesion score 0.09090909090909091 - nodes in this community are weakly interconnected._
- **Should `File Transfer API` be split into smaller, more focused modules?**
  _Cohesion score 0.006153846153846154 - nodes in this community are weakly interconnected._
- **Should `Integration Test Suite` be split into smaller, more focused modules?**
  _Cohesion score 0.03947128947128947 - nodes in this community are weakly interconnected._