# Graph Report - anyr-0.5  (2026-07-21)

## Corpus Check
- 193 files · ~525,243 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 8052 nodes · 24391 edges · 236 communities (219 shown, 17 thin omitted)
- Extraction: 97% EXTRACTED · 3% INFERRED · 0% AMBIGUOUS · INFERRED: 677 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `c49a73fd`
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
- Object Payload Models
- Availability Verification
- Object Accessors
- Chat Example CLI
- Object Update Examples
- HTTP Request Methods
- object_edit.rs
- HTTP Metrics Reporting
- object_edit.rs
- Object Layout Tests
- object_edit.rs
- String
- Error Types
- Object CRUD Requests
- Self
- Chat Read State
- Object List Pagination
- Example Table Rendering
- Object
- String
- Self
- Chat CRUD Tests
- Agenda Example
- mod.rs
- Value
- Value
- File Example
- String
- Basic Filters Example
- Interactive Auth Example
- Value
- stdio.rs
- Type Property Example
- stdio.rs
- Consistency Retry Example
- stdio.rs
- Space Search Example
- p1_cross_space.rs
- index.rs
- p1_cross_space.rs
- String
- .new
- with_test_context_unit
- files.rs
- TestContext
- with_test_context_unit
- decode.rs
- Option
- mod.rs
- create_object_with_retry
- PaginatedResponse<T>
- find_list_object
- NewTagRequest
- unique_test_name
- Widget
- QuickOption
- Style
- VerticalAlign
- priority_groups.rs
- chat.rs
- ViewListObjectsRequest
- Vec
- view_handlers.rs
- mock.rs
- ProcessWatcher
- stdio_conformance.rs
- Member
- pagination_limit
- main.rs
- String
- spaces.rs
- resources.rs
- anyr
- ObjectSummary
- ArchiveReader
- ui.rs
- String
- schema.rs
- Self
- auth.rs
- auth.rs
- File
- object_output.rs
- execute_object_import_batches
- ListTemplatesRequest
- Result
- $defs
- discovery.rs
- .in_space
- AnytypeGrpcClient
- Text
- view.rs
- TestAnyrCommands
- validation.rs
- Changelog
- handle
- AnytypeGrpcError
- views.rs
- chat_messages.rs
- .backup_space
- ensure_list_object
- parse_filters
- auth.rs
- FileContentResponse
- AnytypeError
- error.rs
- handle
- crypto.rs
- test_chat_stream.rs
- Change
- handler_support.rs
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
- anyback
- handle
- AnyMcpServer
- object_create.rs
- init-cli-keys.sh
- Changelog
- Anytype gRPC client
- HandlerError
- init_tracing
- render_table
- verify.rs
- main
- main
- anytype-nonet
- logging.rs
- raycast-edit-anytype.sh
- .new
- server.rs
- config.rs
- test_collect_all_matches_total
- PageLimit
- properties.rs
- .listen_session_events
- http_client.rs
- Anytype Rust Tools and Clients
- prune-templates-keep-oldest.sh
- IdempotencyKey
- .new
- DiscoveryReference
- properties
- Result
- .new
- properties
- headless_integration.rs
- _meta
- properties
- properties
- Result
- FilesClient<'a>
- protocol.rs
- runtime.rs
- properties
- type
- Self
- mutation_value.rs
- execute_create
- execute_object_import_batches
- Result
- MutationDate
- Attempt
- Result
- CompleteResult
- ProjectedDate
- tools
- MutationIcon
- .call_tool
- ElicitRequestFormParams
- .matches_returned
- .new
- String
- jsonrpc
- properties
- run_smoke_tests
- MutationIds
- CancelledNotificationParams
- test_retry_helpers.rs
- load_headless_config
- .with_interceptor
- MutationNumber
- RestCollectionViewFilter
- Live-test mutation rate-limit audit
- README.md

## God Nodes (most connected - your core abstractions)
1. `Result` - 1435 edges
2. `Request` - 418 edges
3. `Response` - 399 edges
4. `ClientCommandsClient<T>` - 343 edges
5. `Status` - 340 edges
6. `Error` - 129 edges
7. `with_test_context()` - 113 edges
8. `with_test_context_unit()` - 107 edges
9. `HttpClient` - 103 edges
10. `AnytypeError` - 100 edges

## Surprising Connections (you probably didn't know these)
- `verify_archive_state_with()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_archive.rs → anytype-api/src/verify.rs
- `execute_create()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_create.rs → anytype-api/src/verify.rs
- `object_edit()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_edit.rs → anytype-api/src/verify.rs
- `object_update()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_update.rs → anytype-api/src/verify.rs
- `headless_default_discovery_routes_paginate_and_report_ambiguity()` --calls--> `with_test_context()`  [INFERRED]
  any-mcp/src/server/headless_integration.rs → anytype-api/src/test_util.rs

## Import Cycles
- None detected.

## Hyperedges (group relationships)
- **Authentication and Keystore Flow** — anytype_api_readme_anytype_api_client, anytype_api_keystores_interactive_authentication, anytype_api_keystores_authentication_token_storage, anytype_api_keystores_endpoint_specific_tokens [EXTRACTED 1.00]
- **gRPC Feature Surface** — anytype_api_changelog_grpc_backend, anytype_api_readme_grpc_api_extensions, anytype_api_readme_files_api, anytype_api_readme_chat_streaming, anytype_api_examples_readme_grpc_examples [EXTRACTED 1.00]

## Communities (236 total, 17 thin omitted)

### Community 0 - "Chat Mock Server"
Cohesion: 0.07
Nodes (28): DataModel, authorize_template_resource(), cleanup_template_resource(), collection_fixture_ownership_error(), complete_type_object_id_snapshot(), current_space_create_response_never_enters_deletion_registry(), delete_space_fixture(), generic_pre_registered_id_cannot_claim_collection_provenance() (+20 more)

### Community 1 - "File Transfer API"
Cohesion: 0.01
Nodes (323): AccountSelectTrace, AddFeatured, AddMessage, AddNotificationSubscriber, Ai, AnyNameAllocate, AnyNameIsValid, AnystoreObjectChanges (+315 more)

### Community 2 - "Space Pagination"
Cohesion: 0.06
Nodes (51): App, build_yaml_front_matter(), CachedObject, clamp_scroll(), detail_string(), detail_value_to_string(), epoch_to_rfc3339(), extract_space_id_from_archive_name() (+43 more)

### Community 3 - "Integration Test Suite"
Cohesion: 0.04
Nodes (7): ClientCommandsClient<T>, Request, Response, Status, View, IntoRequest, Params

### Community 4 - "Authentication API"
Cohesion: 0.08
Nodes (124): add_to_list(), archive_file_paths(), archive_markdown_blob(), archive_object_ids(), archive_payload_file_paths(), assert_non_tty_output_clean(), backup_selected_ids(), ChatMessageTokenCleanupGuard (+116 more)

### Community 5 - "Chat Stream Builder"
Cohesion: 0.11
Nodes (16): ChatHttpEventStream, ChatHttpSseState, eof_finalization_moves_event_buffer_out_of_terminal_state(), grpc_attachments(), grpc_message_content(), grpc_message_conversion_retains_rich_state(), BoxStream, Bytes (+8 more)

### Community 6 - "Filtering and Sorting"
Cohesion: 0.18
Nodes (22): cancellation_releases_permit_for_next_operation(), concurrency_limit_bounds_waiting_operations(), eof_before_initialize_is_a_clean_shutdown(), execute_applies_end_to_end_timeout(), execute_honors_request_cancellation(), initialized_transport_shuts_down_cleanly_on_eof(), operation_diagnostic_classifies_every_outcome(), operation_diagnostic_contains_only_safe_bounded_context() (+14 more)

### Community 7 - "Object Models Utilities"
Cohesion: 0.23
Nodes (13): format_since_display(), parse_local_naive(), parse_local_since(), parse_since(), parse_since_accepts_local_time_without_timezone(), parse_since_accepts_partial_date_variants_equivalently(), parse_since_accepts_plus_zero_suffix(), parse_since_accepts_rfc3339_with_offset() (+5 more)

### Community 8 - "Pagination Core"
Cohesion: 0.16
Nodes (19): bool_field(), ChatMessagesPage, ChatState, HttpMessageWriteAttachment, HttpMessageWriteBody, HttpMessageWriteMark, last_modified_date(), number_field() (+11 more)

### Community 9 - "Client Configuration"
Cohesion: 0.07
Nodes (80): ArchiveObjectInfo, build_archive_object_index(), convert_archive_object_pb_to_markdown(), convert_archive_object_to_markdown(), convert_archive_snapshot_to_markdown(), convert_pb_json_snapshot_to_markdown(), convert_pb_snapshot_to_markdown(), convert_sample_pb_json_object_to_markdown_contains_headings() (+72 more)

### Community 10 - "Member Integration Tests"
Cohesion: 0.22
Nodes (13): ControlledFailureKind, ControlledOperationError, default_control_failure_diagnostic(), log_classified_operation(), OperationContext, OperationFailureDiagnostic, C, CancellationToken (+5 more)

### Community 11 - "Type Request Models"
Cohesion: 0.17
Nodes (10): AnytypeClient, BackoffPolicy, ChatStreamBuilder, get_messages_after(), AnytypeClient, Default, Duration, Pin (+2 more)

### Community 12 - "Property Setter Tests"
Cohesion: 0.25
Nodes (10): call_subscribe_last_messages(), ChatStreamWorker, AnytypeGrpcClient, HashMap, Option, subscribe_previews(), unsubscribe_chat(), unsubscribe_previews() (+2 more)

### Community 13 - "Test Retry Helpers"
Cohesion: 0.06
Nodes (33): AnytypeClient, Color, CreateObjectRequestBody, Icon, ListObjectsRequest, NewObjectRequest, Object, object_link() (+25 more)

### Community 14 - "Client Cache"
Cohesion: 0.04
Nodes (110): Align, Align, Amend, Auth, AutoArchive, AutoRestore, Avatar, BackgroundColor (+102 more)

### Community 15 - "Process Watcher"
Cohesion: 0.15
Nodes (9): diagnostic_path(), diagnostic_path_keeps_only_bounded_non_control_path_context(), has_invalid_percent_encoding(), HttpMetricsSnapshot, parse_diagnostic_target(), Display, Formatter, Option (+1 more)

### Community 16 - "Changelog Concepts"
Cohesion: 0.20
Nodes (16): all_property_object_value(), all_property_type_value(), contract_is_strict_bounded_non_null_and_uses_create_annotations(), is_canonical_plain_body(), is_plain_body_line(), normalize_create_body(), object_create_tool(), object_inner() (+8 more)

### Community 17 - "Chat Resolution Client"
Cohesion: 0.20
Nodes (16): handle(), AppContext, MemberArgs, parse_bool(), parse_condition(), parse_filter(), parse_filters(), parse_number() (+8 more)

### Community 18 - "Tag API"
Cohesion: 0.16
Nodes (19): chat_events_from_event(), chat_events_respect_sub_ids(), ChatEvent, ChatEventStream, ChatStreamControl, ChatStreamHandle, ChatSubscription, ControlMessage (+11 more)

### Community 19 - "Property Value Models"
Cohesion: 0.15
Nodes (7): file_type_filter(), file_type_from_mime(), FilePreloadRequest, FileSource, FileType, FileUploadRequest, upload_uses_rest()

### Community 20 - "Chat Message Models"
Cohesion: 0.15
Nodes (13): Authenticated stdio runtime, Document resources, Exact-match object edit workflow, Object archive workflow, Object create workflow, Object discovery and reads, Object update workflow, Phase 1 foundations (+5 more)

### Community 21 - "Member Models"
Cohesion: 0.20
Nodes (15): create_test_request(), String, test_collect_all_empty(), test_collect_all_single_page(), test_debug_implementation(), test_deref_len_and_is_empty(), test_deref_to_paginated_response(), test_http_request_with_pagination() (+7 more)

### Community 22 - "View Models"
Cohesion: 0.15
Nodes (14): CreateName, CreateReference, optional_body_schema(), optional_icon_schema(), optional_idempotency_schema(), optional_name_schema(), optional_properties_schema(), optional_template_schema() (+6 more)

### Community 23 - "Identifier Resolution"
Cohesion: 0.04
Nodes (85): F, with_test_context_unit(), test_collect_all(), test_create_custom_property(), test_create_multiple_objects(), test_create_with_empty_name(), test_global_search(), test_invalid_object_id() (+77 more)

### Community 24 - "Property Request Builder"
Cohesion: 0.33
Nodes (5): Added, Changed, Changed, Changelog, [Unreleased]

### Community 25 - "HTTP Retry Client"
Cohesion: 0.04
Nodes (60): assert_backup_args_equal(), AuthArgs, backup_export_options(), backup_export_options_maps_include_flags_and_pb_json(), backup_export_options_maps_markdown_include_properties(), backup_target_always_uses_zip_extension_for_generated_name(), backup_target_dest_must_not_exist(), backup_target_dir_must_exist() (+52 more)

### Community 26 - "Property Lookup Helpers"
Cohesion: 0.07
Nodes (96): invalid_catalog_profile_fails_before_auth_without_echoing_its_value(), invalid_operational_setting_does_not_echo_its_value(), invalid_protocol_mode_fails_before_auth_without_echoing_its_value(), invalid_read_only_setting_fails_before_auth_without_echoing_its_value(), startup_auth_failure_is_nonzero_stderr_only_and_redacted(), unauthenticated_command(), add_to_list(), alpha_suffix() (+88 more)

### Community 27 - "Chat RPC Responses"
Cohesion: 0.07
Nodes (53): append_sse_byte(), chat_stream_diagnostic_omits_url_credentials_query_and_fragment(), chat_stream_diagnostic_path(), ChatEditTextRequest, ChatMessageSearchResult, collect_sse_frames(), current_http_message_schema_preserves_available_fields(), delimiter_free_megabyte_uses_incremental_boundary_detection() (+45 more)

### Community 28 - "Object Creation Builder"
Cohesion: 0.15
Nodes (12): &'a mut PaginatedResponse<T>, &'a PagedResult<T>, &'a PaginatedResponse<T>, next_response_iter(), PagedResult, Refill, Arc, IntoIterator (+4 more)

### Community 29 - "Search API"
Cohesion: 0.03
Nodes (34): AutofillMode, Code, Context, DetailsSet, DeviceAdd, DeviceState, GenericErrorResponse, Language (+26 more)

### Community 30 - "Chat Attachments Reactions"
Cohesion: 0.17
Nodes (10): convert_object(), ObjectResourceRead, ResourceConversionError, ResourceOperationError, Display, Formatter, Object, ReadResourceResult (+2 more)

### Community 31 - "Message Content Formatting"
Cohesion: 0.07
Nodes (27): AnytypeClient, CreateTypeProperty, CreateTypeRequestBody, deserialize_vec_properties_or_null(), ListTypesRequest, NewTypeRequest, prime_cache_types(), Arc (+19 more)

### Community 32 - "Input Validation"
Cohesion: 0.27
Nodes (4): set_property_tags(), refresh_cached_property_tags(), resolve_verify(), verify_available()

### Community 33 - "Template API"
Cohesion: 0.05
Nodes (39): Condition, deserialize_vec_string_or_null(), Filter, FilterExpression, FilterOperator, join_values(), Query, QueryWithFilters (+31 more)

### Community 34 - "Type Models"
Cohesion: 0.06
Nodes (79): active_and_archived_scans_stop_at_explicit_page_and_item_bounds(), ambiguous_delete_failures_are_indeterminate_after_one_dispatch(), ambiguous_success_responses_recover_or_return_indeterminate_without_redelete(), archive_evidence(), archive_output(), archive_verification_config(), archive_verification_honors_hard_attempt_and_time_caps(), ArchivedState (+71 more)

### Community 35 - "Cache Controls"
Cohesion: 0.10
Nodes (18): Arc<HttpClient>, deserialize_json(), GetPaged, HttpClient, HttpMetrics, is_idempotent_method(), log_and_backoff(), RawHttpResponse (+10 more)

### Community 36 - "Object Payload Models"
Cohesion: 0.67
Nodes (3): main(), Box, run()

### Community 37 - "Availability Verification"
Cohesion: 0.05
Nodes (78): Account, AppInfo, Attachment, Auth, Bookmark, Chat, ChatMessage, ChatState (+70 more)

### Community 38 - "Object Accessors"
Cohesion: 0.20
Nodes (10): Accessibility Permissions, any-edit: Edit Anytype document in external editor, Build from source, Commands, Configure, Install, License, Platform compatibility (+2 more)

### Community 39 - "Chat Example CLI"
Cohesion: 0.20
Nodes (10): anyr, Build from source, Common options, Configure, Examples, Generating and saving credentials, Install, License (+2 more)

### Community 40 - "Object Update Examples"
Cohesion: 0.09
Nodes (64): archive_file_listing(), archive_signature(), AttachmentCaseBatch, BatchArtifacts, choose_writable_chat_space(), choose_writable_spaces(), cleanup_by_prefix(), cleanup_source_ids() (+56 more)

### Community 41 - "HTTP Request Methods"
Cohesion: 0.08
Nodes (24): chat_details_keys(), chat_message_path(), ChatHttpAddMessageRequest, ChatHttpEditMessageRequest, ChatHttpEvent, ChatMessage, ChatSendTextRequest, filter_unread_messages() (+16 more)

### Community 42 - "object_edit.rs"
Cohesion: 0.07
Nodes (72): apply_edits(), bounded_result(), checked_space_id(), contract_is_closed_bounded_destructive_and_defaults_match_count(), edit_input(), edited_state_matches(), EditExecution, EditInputError (+64 more)

### Community 43 - "HTTP Metrics Reporting"
Cohesion: 0.18
Nodes (12): PagedResult<T>, PaginatedResponse, BoxStream, Ok, S, Self, Serialize, T (+4 more)

### Community 44 - "object_edit.rs"
Cohesion: 0.24
Nodes (4): Self, String, Vec, TestResultTracker

### Community 45 - "Object Layout Tests"
Cohesion: 0.07
Nodes (47): Account, Add, Block, BlockField, Cafe, ChatPreview, Config, Details (+39 more)

### Community 46 - "object_edit.rs"
Cohesion: 0.31
Nodes (3): GrpcSession, open_session_events(), Streaming

### Community 48 - "Error Types"
Cohesion: 0.25
Nodes (8): any-mcp, Build, Headless integration tests, License, Protocol channel, Quick start, Source layout, Testing

### Community 49 - "Object CRUD Requests"
Cohesion: 0.03
Nodes (29): Align, Block, BlockMetaOnly, CardStyle, Condition, Config, DataviewRestriction, Description (+21 more)

### Community 51 - "Chat Read State"
Cohesion: 0.07
Nodes (29): KeyStoreError, From, PathBuf, Self, VarError, default_platform_keyring(), fmt_masked(), GrpcCredentials (+21 more)

### Community 52 - "Object List Pagination"
Cohesion: 0.09
Nodes (50): AuthArgs, AuthCommand, AuthSource, AuthStatusArgs, build_yaml_export(), ConfigFile, detect_scope(), ExportHeaderFormat (+42 more)

### Community 53 - "Example Table Rendering"
Cohesion: 0.02
Nodes (48): ActionType, Amount, AttachmentType, Cart, CartProduct, Code, CryptoCheckout, Data (+40 more)

### Community 54 - "Object"
Cohesion: 0.33
Nodes (6): Automated harness, Client configuration evidence, Current status, External tool evidence, Released compatibility matrix, Stdio protocol verification

### Community 56 - "Self"
Cohesion: 0.29
Nodes (6): Output, OutputFormat, Option, PathBuf, Self, T

### Community 57 - "Chat CRUD Tests"
Cohesion: 0.05
Nodes (36): AddChatMessageResponse, AnytypeClient, chat_search(), chat_search_space(), ChatClient<'a>, ChatCreateRequest, ChatDeleteMessageRequest, ChatGetMessageRequest (+28 more)

### Community 58 - "Agenda Example"
Cohesion: 0.09
Nodes (53): backup_selected_ids(), BugDisposition, CaseStatus, choose_two_distinct_writable_spaces_cli(), choose_writable_space_cli(), clone_sqlite_with_sidecars(), cloned_test_keystore(), configure_test_keystore() (+45 more)

### Community 59 - "mod.rs"
Cohesion: 0.21
Nodes (6): Display, Formatter, Self, Tool, ServerBuildError, validate_catalog()

### Community 60 - "Value"
Cohesion: 0.27
Nodes (9): deserialize_vec_filter_or_null(), deserialize_vec_sort_or_null(), D, Filter, Sort, Vec, View, ViewAddObjectsRequest (+1 more)

### Community 61 - "Value"
Cohesion: 0.18
Nodes (9): Checkbox, Date, Detail, Placeholder, RelationWithValue, RelationWithValuePerObject, Status, Tag (+1 more)

### Community 62 - "File Example"
Cohesion: 0.67
Nodes (3): TestResult, test_chat_discovery_requests(), test_rest_chat_messages_reactions_search_and_reads()

### Community 63 - "String"
Cohesion: 0.67
Nodes (3): TestResult, test_chat_message_crud(), test_rest_chat_message_crud()

### Community 64 - "Basic Filters Example"
Cohesion: 0.10
Nodes (20): AnytypeCache, Arc, AsRef, Default, Formatter, HashMap, Mutex, Option (+12 more)

### Community 65 - "Interactive Auth Example"
Cohesion: 0.07
Nodes (20): AnytypeClient, ClientConfig, extract_port(), find_grpc(), lsof_listen_ports(), lsof_listen_ports_filters_prefix(), probe_grpc_port(), ResponseLimits (+12 more)

### Community 66 - "Value"
Cohesion: 0.67
Nodes (3): TestResult, test_collect_all_matches_total(), test_stream_matches_collect_all()

### Community 67 - "stdio.rs"
Cohesion: 0.06
Nodes (75): ServeError, add_cache(), add_complete(), bounded_reader_recovers_at_the_next_line(), cancel_all(), decode(), dispatch_modern(), drain_frame() (+67 more)

### Community 68 - "Type Property Example"
Cohesion: 0.67
Nodes (3): deserialize_vec_or_null(), D, T

### Community 70 - "Consistency Retry Example"
Cohesion: 0.14
Nodes (12): ChatAddMessageRequest, ChatEditMessageRequest, MessageBlock, MessageContent, AsRef, Color, MessageBlockEditorQuote, MessageBlockEmbed (+4 more)

### Community 71 - "stdio.rs"
Cohesion: 0.22
Nodes (7): CancellationToolServer, Arc, Notify, RequestContext, ServerHandler, ServerInfo, McpError

### Community 72 - "Space Search Example"
Cohesion: 0.11
Nodes (46): with_test_context(), is_expected_member_lookup_error(), String, TestResult, test_active_member_exists(), test_get_member_by_id(), test_get_member_invalid_space(), test_get_nonexistent_member() (+38 more)

### Community 73 - "p1_cross_space.rs"
Cohesion: 0.61
Nodes (8): initialize_protocol(), read_frame(), request(), write_frame(), BufReader, DuplexStream, ReadHalf, WriteHalf

### Community 74 - "index.rs"
Cohesion: 0.13
Nodes (38): ArchiveIndex, build_preview(), build_preview_is_stable_and_compact(), build_preview_preserves_markdown_lines_without_truncating_headings(), collect_link_candidates(), collect_preview_strings(), collect_user_properties(), collect_user_properties_includes_array_and_object_values() (+30 more)

### Community 75 - "p1_cross_space.rs"
Cohesion: 0.25
Nodes (8): type, stopSequences, supportedVersions, items, type, description, items, type

### Community 76 - "String"
Cohesion: 0.25
Nodes (8): ChatArgs, Commands, FileArgs, AuthArgs, Box, ListArgs, ObjectArgs, SpaceArgs

### Community 77 - ".new"
Cohesion: 0.20
Nodes (15): ChatClient, ChatHttpMessageStreamRequest, dropping_stream_cancels_incomplete_transport(), mock_http_client(), one_transport_chunk_can_carry_multiple_exact_limit_events(), opening_transport_failure_discards_raw_url_and_source(), overflowing_stream_terminates_and_releases_transport_state(), rest_add_message_sends_current_wire_shape() (+7 more)

### Community 79 - "files.rs"
Cohesion: 0.09
Nodes (43): file_from_details(), file_from_http_upload(), file_http_metadata(), FileHttpMetadata, FileObject, FilesClient, FileStyle, FileUploadResponse (+35 more)

### Community 80 - "TestContext"
Cohesion: 0.13
Nodes (29): collection_object(), collection_view_fixture_rejects_missing_default_without_indexing_it(), complete_global_template_owners(), complete_space_id_snapshot(), complete_space_object_ids(), complete_template_ids(), complete_template_objects(), complete_template_ownership_snapshot() (+21 more)

### Community 82 - "decode.rs"
Cohesion: 0.12
Nodes (46): build_expanded_entry_from_details(), derive_layout_name(), detail_value(), ExpandedSnapshotEntry, format_datetime_display(), format_datetime_with_tz(), format_last_modified(), format_utc_datetime_with_tz() (+38 more)

### Community 83 - "Option"
Cohesion: 0.43
Nodes (4): FixtureReply, Duration, Self, Value

### Community 84 - "mod.rs"
Cohesion: 0.19
Nodes (10): PropertyArgs, choose_property_name(), handle(), handle_update(), property_command(), AppContext, Option, String (+2 more)

### Community 85 - "create_object_with_retry"
Cohesion: 0.12
Nodes (40): create_object_with_retry(), ensure_properties_and_type(), is_key_already_exists_error(), lookup_property_tag_with_retry(), F, Object, Tag, TestResult (+32 more)

### Community 86 - "PaginatedResponse<T>"
Cohesion: 0.29
Nodes (3): PaginatedResponse<T>, Iter, IterMut

### Community 87 - "find_list_object"
Cohesion: 0.50
Nodes (4): find_list_object(), main(), Object, Option

### Community 88 - "NewTagRequest"
Cohesion: 0.08
Nodes (28): AnytypeClient, CreateTagRequest, ListTagsRequest, NewTagRequest, Arc, Color, Filter, Into (+20 more)

### Community 89 - "unique_test_name"
Cohesion: 0.12
Nodes (46): retry_definitive_rate_limit(), unique_test_name(), TestResult, test_create_custom_property(), test_create_property_duplicate_key(), test_create_property_invalid_name(), test_delete_property(), test_property_key_stability() (+38 more)

### Community 90 - "Widget"
Cohesion: 0.50
Nodes (4): Limit, ViewId, Widget, Layout

### Community 95 - "priority_groups.rs"
Cohesion: 0.09
Nodes (40): assert_case_registered(), case_replace_after_object_type_changed_since_backup(), case_replace_object_type_collection_with_items(), case_replace_object_type_complex_nested_object(), case_replace_object_type_custom_type_object(), case_replace_object_type_file(), case_replace_object_type_object(), case_replace_object_type_property() (+32 more)

### Community 98 - "chat.rs"
Cohesion: 0.13
Nodes (31): decode_order_id_arg(), decode_order_id_hex_roundtrip(), decode_order_id_non_hex_passthrough(), emit_message_rows(), encode_order_id_hex(), format_sender(), handle(), hex_char() (+23 more)

### Community 99 - "ViewListObjectsRequest"
Cohesion: 0.21
Nodes (12): AnytypeClient, fixture_client(), ListViewsRequest, Arc, Into, IntoIterator, Option, S (+4 more)

### Community 100 - "Vec"
Cohesion: 0.07
Nodes (39): BlockParticipant, ChangePayload, Dataview, DataviewRestrictions, DetailsSet, HistorySize, IdentityProfile, IdentityProfileWithKey (+31 more)

### Community 101 - "view_handlers.rs"
Cohesion: 0.07
Nodes (48): ambiguous_view_name_returns_actionable_bounded_candidates(), convert_view_object_page(), convert_view_page(), fixture_client(), fixture_server(), object(), page(), read_request() (+40 more)

### Community 102 - "mock.rs"
Cohesion: 0.05
Nodes (85): authenticate(), broadcast_event(), build_chat_state(), build_event(), chat_add_value(), chat_delete_value(), chat_details(), chat_state_update_value() (+77 more)

### Community 103 - "ProcessWatcher"
Cohesion: 0.13
Nodes (25): Account, matches_process_kind(), next_test_addr(), open_session_events(), ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatcher (+17 more)

### Community 104 - "stdio_conformance.rs"
Cohesion: 0.08
Nodes (51): assert_compact_wire_catalog(), assert_exact_decoder_error(), assert_exact_wire_catalog(), assert_exchange_depth(), assert_official_modern_request(), assert_official_modern_response(), assert_stdout_purity(), assert_structured_result() (+43 more)

### Community 107 - "Member"
Cohesion: 0.10
Nodes (17): AnytypeClient, ListMembersRequest, make_member(), Member, MemberRequest, MemberResponse, MemberRole, MemberStatus (+9 more)

### Community 110 - "pagination_limit"
Cohesion: 0.11
Nodes (19): handle(), list_command(), list_objects_accepts_view(), list_objects_requires_view(), AppContext, ListArgs, pagination_limit(), pagination_offset() (+11 more)

### Community 113 - "main.rs"
Cohesion: 0.16
Nodes (29): init_logging(), auth_login(), auth_logout(), AuthCommand, check_auth_status(), Cli, Commands, copy_link_command() (+21 more)

### Community 114 - "String"
Cohesion: 0.12
Nodes (45): ChatReadTypeArg, AppContext, AuthArgs, AuthCommands, build_client(), ChatCommands, ChatMessagesArgs, ChatMessagesCommands (+37 more)

### Community 116 - "spaces.rs"
Cohesion: 0.05
Nodes (43): AnytypeClient, archived_object_from_search_result(), archived_relation_not_found(), archived_search_request(), BackupSpaceRequest, CreateSpaceRequestBody, dataview_filter_checkbox_equal(), dataview_filter_type_in() (+35 more)

### Community 117 - "resources.rs"
Cohesion: 0.15
Nodes (36): AnytypeResources, body_above_100k_chars_fails_without_silent_truncation(), cancellation_aborts_a_delayed_resource_read(), canonical_search_get_and_write_uri_type_round_trips_strictly(), controlled_error(), document_response_byte_ceiling_is_exact_before_conversion(), error_code(), exact_get_returns_complete_100k_unicode_body_and_bounded_metadata() (+28 more)

### Community 121 - "anyr"
Cohesion: 0.17
Nodes (12): Before Submitting a Bug Report, Before Submitting an Enhancement, Contributing to stevelr/anytype, Documentation, How Do I Submit a Good Bug Report?, How Do I Submit a Good Enhancement Suggestion?, I Have a Question, I Want To Contribute (+4 more)

### Community 122 - "ObjectSummary"
Cohesion: 0.06
Nodes (27): BoundedText<MAX>, DomainValueError, LastModified, LastModifiedError, object_summary_serializes_with_canonical_resource_uri(), ObjectResourceUri, ObjectSummary, AsRef (+19 more)

### Community 123 - "ArchiveReader"
Cohesion: 0.13
Nodes (20): ArchiveFileEntry, ArchiveReader, ArchiveSourceKind, infer_object_id_from_snapshot_path(), infer_object_ids_from_files(), looks_like_content_id(), reader_lists_and_reads_directory_archive(), reader_lists_and_reads_zip_archive() (+12 more)

### Community 124 - "ui.rs"
Cohesion: 0.15
Nodes (33): buffer_to_lines(), draw(), draw_contents_panel(), draw_footer(), draw_help_overlay(), draw_links_panel(), draw_main_area(), draw_metadata_panel() (+25 more)

### Community 125 - "String"
Cohesion: 0.11
Nodes (17): column_widths(), FileObject, format_row(), format_separator(), Member, Object, Property, render_table() (+9 more)

### Community 127 - "schema.rs"
Cohesion: 0.07
Nodes (54): BooleanTrueInput, BooleanTrueSchema, contains_keyword(), EmptyNestedInput, EmptySchema, FreeFormMapInput, FreeFormValueInput, has_only_keywords() (+46 more)

### Community 131 - "Self"
Cohesion: 0.15
Nodes (7): FileListRequest, FileSearchRequest, filter_not_empty(), Filter, IntoIterator, Self, size_filter()

### Community 134 - "auth.rs"
Cohesion: 0.14
Nodes (14): AnytypeClient, AuthStatus, CreateApiKeyRequest, CreateApiKeyResponse, CreateChallengeRequest, CreateChallengeResponse, GrpcStatus, HttpStatus (+6 more)

### Community 137 - "auth.rs"
Cohesion: 0.18
Nodes (20): create_local_link_challenge(), create_session(), create_session_token(), create_session_token_from_account_key(), create_session_token_from_app_key(), LocalLinkCredentials, AsRef, Channel (+12 more)

### Community 138 - "File"
Cohesion: 0.11
Nodes (16): Bookmark, Description, FaviconHash, File, Hash, ImageHash, Mime, Name (+8 more)

### Community 141 - "object_output.rs"
Cohesion: 0.07
Nodes (52): all_properties(), all_property_variants_have_closed_bounded_wire_forms(), bounded(), bounded_date(), bounded_values(), cursor_projection_normalization_is_order_insensitive(), last_modified(), malformed_oversized_and_duplicate_selected_values_fail_closed() (+44 more)

### Community 143 - "execute_object_import_batches"
Cohesion: 0.08
Nodes (52): ObjectDescriptor, apply_import_response(), archive_basename(), ArchiveCmpChanged, ArchiveCmpObject, ArchiveCmpReport, AuthCommands, BackupSelection (+44 more)

### Community 147 - "ListTemplatesRequest"
Cohesion: 0.19
Nodes (12): AnytypeClient, ListTemplatesRequest, Arc, Filter, Into, Object, Option, Self (+4 more)

### Community 148 - "Result"
Cohesion: 0.23
Nodes (28): exit_code(), main(), Box, T, with_token(), auth_status(), connect(), disable_sharing() (+20 more)

### Community 149 - "$defs"
Cohesion: 0.04
Nodes (68): description, properties, type, description, properties, type, description, properties (+60 more)

### Community 154 - "discovery.rs"
Cohesion: 0.08
Nodes (60): assert_error(), convert_tag_summary(), convert_type_summary(), cursor_from(), DiscoveryHandlers, domain_handler_error(), EmptyPageParams, enabled_toolsets() (+52 more)

### Community 155 - ".in_space"
Cohesion: 0.18
Nodes (10): chat_layout_filter(), ChatHttpListRequest, filter_id_equal(), filter_name_equal(), HttpChatEventEnvelope, request_json(), Filter, Value (+2 more)

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

### Community 166 - "validation.rs"
Cohesion: 0.08
Nodes (29): body_limits_and_exact_unicode_boundaries(), BodyCharLimit, BodyChunk, BodyOffset, BoundedList, BoundedList<T, MAX>, chunk_body(), error() (+21 more)

### Community 170 - "Changelog"
Cohesion: 0.09
Nodes (21): [0.2.2] - anyr - 2026-01-12, [0.2.3] - anyr - 2026-01-12, [0.2.4] - anyr - 2026-01-17, [0.3.0] - anyr - 2026-01-28, [0.4.0] - anyr - 2026-02-16, [0.4.1], Added, Added (+13 more)

### Community 171 - "handle"
Cohesion: 0.19
Nodes (15): apply_file_filters_list(), apply_file_filters_search(), download_http(), FileType, handle(), merge_properties(), parse_properties(), AppContext (+7 more)

### Community 172 - "AnytypeGrpcError"
Cohesion: 0.29
Nodes (12): AnytypeGrpcError, AuthError, BackupError, ConfigError, ConfigError, From, PathBuf, Self (+4 more)

### Community 173 - "views.rs"
Cohesion: 0.17
Nodes (21): BlockDataview, ClientCommandsClient, authenticated_request(), fetch_grid_view_columns(), find_dataview_block(), GridViewColumn, GridViewInfo, invalid_view_token_is_preserved_as_typed_auth_error() (+13 more)

### Community 179 - "chat_messages.rs"
Cohesion: 0.23
Nodes (16): Cli, Commands, format_order_id(), hex_to_bytes(), hex_value(), is_hex(), last_five_chars(), list_chats() (+8 more)

### Community 180 - ".backup_space"
Cohesion: 0.22
Nodes (12): AnytypeGrpcClient, generated_target_name(), ExportFormat, Into, PathBuf, Self, String, Vec (+4 more)

### Community 183 - "ensure_list_object"
Cohesion: 0.34
Nodes (15): ObjectLayout, ensure_list_object(), find_list_object_by_layout(), list_views_with_retry(), Object, Option, TestResult, Vec (+7 more)

### Community 185 - "parse_filters"
Cohesion: 0.19
Nodes (16): TypeArgs, handle(), mode_clear_when_clear_flag(), mode_merge_when_add_properties(), mode_replace_parses_set_properties(), mode_unchanged_when_no_flags(), AppContext, String (+8 more)

### Community 189 - "auth.rs"
Cohesion: 0.24
Nodes (14): find_grpc_cmd(), handle(), HeadlessConfig, login(), logout(), AppContext, AuthArgs, Option (+6 more)

### Community 190 - "FileContentResponse"
Cohesion: 0.28
Nodes (4): file_path(), FileContentResponse, Method, StatusCode

### Community 198 - "AnytypeError"
Cohesion: 0.04
Nodes (38): main(), MessageContent, Object, status_color(), main(), main(), main(), main() (+30 more)

### Community 199 - "error.rs"
Cohesion: 0.07
Nodes (31): ambiguity_candidates_are_bounded(), AmbiguityCandidatesError, anytype_classifiers_cover_every_directly_constructible_error_variant(), anytype_error_mapping_discards_upstream_response_text(), AnytypeErrorMapping, api_error(), assert_anytype_mapping(), candidate() (+23 more)

### Community 200 - "handle"
Cohesion: 0.28
Nodes (14): handle(), HeadlessConfig, login(), logout(), AppContext, AuthArgs, Option, PathBuf (+6 more)

### Community 203 - "crypto.rs"
Cohesion: 0.32
Nodes (11): crc16_xmodem(), derive_keys_from_mnemonic(), derive_keys_from_mnemonic_go_vector(), encode_account_id(), String, slip10_derive_child(), slip10_derive_master(), slip10_derive_path() (+3 more)

### Community 206 - "test_chat_stream.rs"
Cohesion: 0.31
Nodes (9): chat_stream_receives_messages(), chat_stream_reconnects_after_disconnect(), rest_chat_stream_receives_initial_message(), AnytypeClient, F, SocketAddr, setup_mock_client(), wait_for_event() (+1 more)

### Community 207 - "Change"
Cohesion: 0.17
Nodes (13): Change, ChangeNoSnapshot, Content, DocumentCreate, DocumentDelete, FileKeys, HashMap, Snapshot (+5 more)

### Community 214 - "handler_support.rs"
Cohesion: 0.08
Nodes (59): assert_error(), begin_page(), BoundedResult, continuation_advances_by_upstream_window_not_sparse_item_count(), contract(), ControlledFailurePolicy, conversion_and_encoding_failures_emit_one_safe_failure_diagnostic(), cursor_binding_includes_tool_limit_and_normalized_non_cursor_params() (+51 more)

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
Cohesion: 0.35
Nodes (10): build_member_identity_map(), load_member_cache(), MemberCache, parse_member_identity(), resolve_member_name(), AppContext, HashMap, Option (+2 more)

### Community 225 - "Changelog"
Cohesion: 0.06
Nodes (41): Ambiguous Resolution Error, Archived Object Management, Changelog, DB Keystore Migration, gRPC Backend, Process Watcher, Resolve Module, Semantic Versioning (+33 more)

### Community 227 - "FileDownloadRequest"
Cohesion: 0.16
Nodes (9): FileDownloadDestination, FileDownloadRequest, FileHttpUploadRequest, rich_and_url_uploads_select_grpc(), AsRef, Bytes, Path, PathBuf (+1 more)

### Community 228 - "ImageResizeSchema"
Cohesion: 0.35
Nodes (11): FileInfo, FileKeys, ImageResizeSchema, Link, HashMap, Link, Option, String (+3 more)

### Community 235 - "fix_doc_list_indents"
Cohesion: 0.42
Nodes (8): fix_doc_list_indents(), indent_doc_list_continuation(), indent_doc_list_line(), main(), Box, Option, PathBuf, String

### Community 239 - "TestResultTracker"
Cohesion: 0.07
Nodes (34): cursor_binding_tampering_and_process_expiry(), cursor_parts(), cursor_registry_evicts_oldest_state_at_its_cap(), CursorStore, CursorStoreError, CursorToken, hex(), hex_nibble() (+26 more)

### Community 241 - "Message"
Cohesion: 0.22
Nodes (9): BlockUpdate, DropFiles, Export, Import, Message, Migration, PreloadFile, Value (+1 more)

### Community 249 - "anyback"
Cohesion: 0.22
Nodes (8): anyback, Commands, Development, Features, Integrity Testing, Library Crate, Restore Transport, Usage Notes

### Community 250 - "handle"
Cohesion: 0.20
Nodes (13): must_have_body(), resolve_icon_exists(), AsRef, Icon, Into, Path, handle(), merge_properties() (+5 more)

### Community 251 - "AnyMcpServer"
Cohesion: 0.16
Nodes (20): AnyMcpServer, decode_arguments(), invalid_arguments(), reject_static_cursor(), Arc, CancellationToken, ErrorData, JsonObject (+12 more)

### Community 253 - "object_create.rs"
Cohesion: 0.33
Nodes (22): all_shared_property_and_icon_forms_reach_one_canonical_create_payload(), first_postdispatch_failures_are_fixed_and_key_retry_never_posts_twice(), fixture(), identical_sequential_and_concurrent_keyed_calls_create_once(), input(), mismatched_key_reuse_and_read_only_cached_call_do_no_io(), named_space_type_and_template_are_bounded_and_revalidated_before_create(), plain_body_normalizes_before_fingerprint_post_and_both_verifications() (+14 more)

### Community 257 - "init-cli-keys.sh"
Cohesion: 0.32
Nodes (5): ANYTYPE_GRPC_ENDPOINT, ANYTYPE_URL, init_cli_and_keystore(), join_space(), init-cli-keys.sh script

### Community 260 - "Changelog"
Cohesion: 0.29
Nodes (6): 0.1.0 - 2026-02-10, [0.3.0 - alpha] - anyback - 2026-02-16, [0.4.0-alpha.2], Changed, Changelog, [Unreleased]

### Community 263 - "Anytype gRPC client"
Cohesion: 0.29
Nodes (6): Anytype gRPC client, Building, Compatibility, License, Related projects, Status and plan

### Community 266 - "HandlerError"
Cohesion: 0.40
Nodes (4): CursorBinding, S, Serialize, Value

### Community 271 - "init_tracing"
Cohesion: 0.15
Nodes (14): Formatter, build_client(), Cli, ColorArg, handle_manifest(), ManifestArgs, resolve_space(), AnytypeClient (+6 more)

### Community 272 - "render_table"
Cohesion: 0.60
Nodes (5): format_row(), format_separator(), render_table(), String, Vec

### Community 274 - "verify.rs"
Cohesion: 0.14
Nodes (26): api_error(), availability_wrapper_preserves_first_success_behavior(), config(), dropped_verifier_drops_in_flight_fetch_without_retaining_value(), exact_attempt_cap_is_enforced_for_zero_delay(), legacy_availability_retry_classifier_preserves_exact_parity(), next_delay(), nonretryable_error_stops_immediately() (+18 more)

### Community 289 - "logging.rs"
Cohesion: 0.09
Nodes (30): build_filter(), Capture, capture_default(), dependency_payload_targets_cannot_override_metadata_deny_filter(), ensure_trace_interest(), has_target_prefix(), init(), LoggingError (+22 more)

### Community 295 - "raycast-edit-anytype.sh"
Cohesion: 0.67
Nodes (3): EDITOR_COMMAND, notify(), raycast-edit-anytype.sh script

### Community 297 - ".new"
Cohesion: 0.07
Nodes (55): ObjectOutput, checkbox_and_number_filters_are_forwarded_without_rewriting(), chunked_get_hashes_complete_unicode_body_without_leaking_remainder(), contracts_are_strict_bounded_and_body_is_optional_non_null(), convert_search_page(), convert_search_response(), decode_and_dispatch_search(), filter_depth_value_and_empty_array_bounds_are_enforced() (+47 more)

### Community 301 - "server.rs"
Cohesion: 0.13
Nodes (31): ApplicationProfile, assert_catalog_contracts(), assert_valid_representative(), audit_schema(), capabilities_are_static_complete_and_never_advertise_list_changed(), catalog_entries_equal_the_original_typed_contracts(), catalog_snapshot(), compact_omissions_are_unknown_while_read_only_edit_fails_closed() (+23 more)

### Community 308 - "config.rs"
Cohesion: 0.14
Nodes (26): application_profile_parser_is_exact_and_secret_safe(), config(), ConfigError, default_document_budget_is_routed_to_anytype_client(), defaults_are_bounded_and_reuse_anyr_keystore_service(), errors_name_the_variable_without_echoing_its_value(), maps_supported_anytype_environment_settings(), non_empty() (+18 more)

### Community 314 - "test_collect_all_matches_total"
Cohesion: 0.16
Nodes (19): array_condition(), ArrayCondition, checkbox_condition(), CheckboxCondition, date_condition(), DateCondition, number_condition(), NumberCondition (+11 more)

### Community 315 - "PageLimit"
Cohesion: 0.09
Nodes (18): PageRequest, non_null_cursor_schema(), page_omits_terminal_cursor_from_json(), Page<T>, PageLimit, PageOffset, PaginationInput, Cow (+10 more)

### Community 322 - "properties.rs"
Cohesion: 0.05
Nodes (58): property(), AnytypeClient, assert_malformed_tag_pagination(), CreatePropertyRequestBody, deserialize_vec_string_or_null(), deserialize_vec_tag_or_null(), direct_property_get_is_cache_independent_and_exactly_scoped(), direct_property_get_validates_both_ids_before_io() (+50 more)

### Community 346 - "http_client.rs"
Cohesion: 0.06
Nodes (68): all_http_trace_levels_remain_metadata_only(), assert_public_mutation_sent_once(), assert_public_redirect_is_not_followed(), caller_supplied_reqwest_retry_policy_cannot_replay_a_mutation(), Capture, chunked_exact_limit_succeeds_without_content_length(), chunked_framing_cannot_bypass_limit_with_a_low_length_header(), content_length_exact_limit_succeeds() (+60 more)

### Community 364 - "IdempotencyKey"
Cohesion: 0.20
Nodes (11): CreateFingerprintV1, fingerprint_hex(), FingerprintField, FingerprintField<'a, T>, NormalizedCreate, ObjectCreateInput, MutationProperties, Option (+3 more)

### Community 366 - ".new"
Cohesion: 0.05
Nodes (94): ambiguity_error_exposes_only_bounded_candidates(), ambiguous(), ambiguous_scans_also_fail_when_candidate_completeness_exceeds_the_limit(), AnytypeClient, bare_chat_id_discovery_has_one_global_space_and_chat_budget(), candidate_is_safe(), candidate_membership_is_independent_of_input_order(), candidate_selection_filters_invalid_values_before_its_cap() (+86 more)

### Community 376 - "DiscoveryReference"
Cohesion: 0.07
Nodes (38): checked_tag_count(), concise_summaries_preserve_closed_fields_only(), convert_property_summary(), convert_space_summary(), DiscoveryReference, finish_property_page(), property_format(), PropertyFormatSummary (+30 more)

### Community 377 - "properties"
Cohesion: 0.07
Nodes (33): description, properties, required, type, description, enum, type, required (+25 more)

### Community 386 - "Result"
Cohesion: 0.26
Nodes (5): FixtureReply, ObjectCreateHandlers, D, Duration, Self

### Community 387 - ".new"
Cohesion: 0.06
Nodes (101): BoundedText, HandlerError, Display, From, BodySha256, checked_effective_type(), checked_space_id(), checked_type_key() (+93 more)

### Community 388 - "properties"
Cohesion: 0.06
Nodes (31): description, properties, required, type, CreateMessageRequestParams, description, enum, type (+23 more)

### Community 401 - "headless_integration.rs"
Cohesion: 0.21
Nodes (27): active_contains(), archived_contains(), arguments(), assert_archive_evidence(), assert_collection_view_continuation(), assert_cursor_continuation(), assert_fixture_space_continuation(), assert_fixture_template_continuation() (+19 more)

### Community 402 - "_meta"
Cohesion: 0.05
Nodes (46): description, properties, required, type, additionalProperties, description, type, description (+38 more)

### Community 403 - "properties"
Cohesion: 0.13
Nodes (15): description, properties, required, type, CreateMessageResult, description, type, model (+7 more)

### Community 419 - "properties"
Cohesion: 0.07
Nodes (28): description, properties, $ref, type, description, properties, required, type (+20 more)

### Community 422 - "Result"
Cohesion: 0.10
Nodes (23): AnytypeReference, FilterNumber, FilterNumberError, optional_body_input_schema(), optional_body_output_schema(), optional_cursor_schema(), optional_filter_schema(), optional_projection_schema() (+15 more)

### Community 427 - "FilesClient<'a>"
Cohesion: 0.11
Nodes (16): AnytypeClient, conditional_not_modified_status_is_preserved(), FileContentRequest, FileDeleteRequest, FileDiscardPreloadRequest, FileGetRequest, FilesClient<'a>, head_returns_file_metadata_without_a_body() (+8 more)

### Community 441 - "protocol.rs"
Cohesion: 0.07
Nodes (36): all_discovery_contracts_are_strict_bounded_read_tools(), property_list_tool(), server_status_tool(), space_list_tool(), tag_list_tool(), template_list_tool(), type_list_tool(), Page (+28 more)

### Community 442 - "runtime.rs"
Cohesion: 0.11
Nodes (17): AnytypeClient, AtomicU64, Display, Duration, Formatter, Self, Semaphore, RuntimeContext (+9 more)

### Community 443 - "properties"
Cohesion: 0.08
Nodes (25): $ref, description, properties, type, ClientCapabilities, description, properties, type (+17 more)

### Community 444 - "type"
Cohesion: 0.09
Nodes (25): properties, required, type, type, BooleanSchema, type, additionalProperties, default (+17 more)

### Community 454 - "Self"
Cohesion: 0.18
Nodes (10): ExpectedRequest, HttpFixture, ReferenceError, BTreeMap, Display, Formatter, Into, JoinHandle (+2 more)

### Community 455 - "mutation_value.rs"
Cohesion: 0.13
Nodes (16): comparison_classifies_bounded_and_malformed_upstream_values(), dates_are_bounded_rfc3339_and_canonical_utc(), ids_cap_raw_input_before_sorting_and_deduplication(), MutationContractInput, normalized_properties(), normalized_values_serialize_deterministically_for_future_fingerprints(), only_documented_empty_values_compare_as_missing_clears(), properties_are_bounded_sorted_and_duplicate_keys_reject() (+8 more)

### Community 474 - "execute_create"
Cohesion: 0.24
Nodes (14): HandlerOperationError, execute_create(), indeterminate_operation(), CancellationToken, EntityId, Object, ObjectId, SpaceId (+6 more)

### Community 477 - "execute_object_import_batches"
Cohesion: 0.12
Nodes (28): aggregate_import_responses(), AppContext, build_import_plan_infers_ids_without_manifest_from_zip(), execute_object_import(), execute_object_import_batches(), execute_object_import_path(), handle_restore_apply(), import_event_timeouts_from_env() (+20 more)

### Community 478 - "Result"
Cohesion: 0.03
Nodes (12): Formatter, Path, run_inspector(), init_tracing(), main(), run(), main(), Status (+4 more)

### Community 490 - "MutationDate"
Cohesion: 0.13
Nodes (10): MutationDate, MutationIconText, MutationPropertyKey, Cow, Deserialize, JsonSchema, Schema, SchemaGenerator (+2 more)

### Community 491 - "Attempt"
Cohesion: 0.20
Nodes (16): Attempt, BeginAttempt, CreateDisposition, CreateExecution, finish_supervised_execution(), IdempotencyKey, IdempotencyStore, Arc (+8 more)

### Community 493 - "Result"
Cohesion: 0.26
Nodes (13): canonical_json(), compact_canonical_json(), numeric_boundary(), HashMap, HashSet, Map, String, Value (+5 more)

### Community 518 - "CompleteResult"
Cohesion: 0.17
Nodes (12): properties, description, type, hasMore, total, values, description, type (+4 more)

### Community 537 - "tools"
Cohesion: 0.11
Nodes (18): description, items, type, items, $ref, items, type, audience (+10 more)

### Community 551 - "MutationIcon"
Cohesion: 0.21
Nodes (8): Color, Icon, MutationColor, MutationIcon, From, Icon, Option, validate_returned_icon_text()

### Community 552 - ".call_tool"
Cohesion: 0.17
Nodes (13): Any, Future, Option, Output, RoleServer, RxJsonRpcMessage, Transport, TxJsonRpcMessage (+5 more)

### Community 553 - "ElicitRequestFormParams"
Cohesion: 0.12
Nodes (17): ElicitRequestFormParams, ElicitRequestURLParams, description, properties, required, type, description, properties (+9 more)

### Community 566 - ".matches_returned"
Cohesion: 0.27
Nodes (9): canonical_returned_date(), MutationCompareError, returned_id(), returned_ids(), returned_tag_ids(), Display, Formatter, Tag (+1 more)

### Community 567 - ".new"
Cohesion: 0.38
Nodes (5): MutationInputError, D, Into, Self, String

### Community 582 - "String"
Cohesion: 0.19
Nodes (11): CreateInputError, fingerprint_v1_is_domain_separated_golden_and_semantically_canonical(), input_with_body(), no_request_fixture(), plain_canonical_expansion_overflow_fails_before_io(), read_request(), Display, Formatter (+3 more)

### Community 585 - "jsonrpc"
Cohesion: 0.30
Nodes (15): required, required, required, required, required, required, required, required (+7 more)

### Community 595 - "properties"
Cohesion: 0.14
Nodes (14): description, format, type, properties, required, type, BlobResourceContents, blob (+6 more)

### Community 597 - "run_smoke_tests"
Cohesion: 0.17
Nodes (16): Instant, Iter, TestContext, TestResults, run_smoke_tests(), smoke_test(), test_filters(), test_members_api() (+8 more)

### Community 629 - "MutationIds"
Cohesion: 0.25
Nodes (7): generic_set_property_application_omits_format_and_preserves_canonical_values(), ids(), MutationIds, MutationProperty, EntityId, R, MutationText

### Community 631 - "CancelledNotificationParams"
Cohesion: 0.06
Nodes (37): properties, description, properties, required, type, description, properties, required (+29 more)

### Community 654 - "test_retry_helpers.rs"
Cohesion: 0.31
Nodes (7): definitive_rate_limit_retry_delay(), Duration, api_error(), definitive_rate_limit_backoff_is_finite_and_bounded(), definitive_rate_limit_retry_exhausts_at_the_finite_attempt_cap(), indeterminate_failure_is_never_retried(), only_typed_http_429_is_a_definitive_retryable_rejection()

### Community 655 - "load_headless_config"
Cohesion: 0.39
Nodes (8): AnytypeHeadlessConfig, default_headless_config_path(), load_headless_config(), ConfigError, Option, Path, PathBuf, String

### Community 700 - ".with_interceptor"
Cohesion: 0.33
Nodes (4): F, T, InterceptedService, Uri

### Community 729 - "MutationNumber"
Cohesion: 0.70
Nodes (3): canonical_returned_number(), MutationNumber, Number

### Community 733 - "RestCollectionViewFilter"
Cohesion: 0.06
Nodes (62): clone_collection_view(), collection_fixture_transport_error(), collection_fixture_transport_error_redacts_tonic_status(), collection_matches_fixture_provenance(), collection_type_details(), collection_type_fixture_details_use_the_canonical_heart_layout(), collection_view_fixture_accepts_exact_new_event_identity(), collection_view_fixture_binds_object_show_root_and_exact_block() (+54 more)

### Community 774 - "Live-test mutation rate-limit audit"
Cohesion: 0.50
Nodes (3): Deliberate exclusions, Live-test mutation rate-limit audit, Retried setup inventory

## Knowledge Gaps
- **686 isolated node(s):** `EDITOR_COMMAND`, `AuthCommand`, `EnabledToolset`, `EmptyPageParams`, `SpacePageParams` (+681 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **17 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Result` connect `Result` to `Space Pagination`, `Integration Test Suite`, `Authentication API`, `Chat Stream Builder`, `Filtering and Sorting`, `Object Models Utilities`, `Client Configuration`, `Member Integration Tests`, `Type Request Models`, `Property Setter Tests`, `Test Retry Helpers`, `Process Watcher`, `Changelog Concepts`, `Chat Resolution Client`, `Tag API`, `HTTP Retry Client`, `Property Lookup Helpers`, `Chat RPC Responses`, `Object Creation Builder`, `Chat Attachments Reactions`, `Message Content Formatting`, `Input Validation`, `Template API`, `Type Models`, `Cache Controls`, `Object Payload Models`, `Availability Verification`, `MutationIcon`, `.call_tool`, `Object Update Examples`, `object_edit.rs`, `HTTP Request Methods`, `HTTP Metrics Reporting`, `object_edit.rs`, `Object CRUD Requests`, `Chat Read State`, `Object List Pagination`, `.matches_returned`, `.new`, `Self`, `Chat CRUD Tests`, `Agenda Example`, `mod.rs`, `String`, `Value`, `Basic Filters Example`, `Interactive Auth Example`, `stdio.rs`, `Type Property Example`, `String`, `stdio.rs`, `index.rs`, `files.rs`, `TestContext`, `decode.rs`, `mod.rs`, `find_list_object`, `NewTagRequest`, `unique_test_name`, `chat.rs`, `ViewListObjectsRequest`, `Vec`, `view_handlers.rs`, `mock.rs`, `ProcessWatcher`, `stdio_conformance.rs`, `Member`, `pagination_limit`, `main.rs`, `String`, `spaces.rs`, `resources.rs`, `ObjectSummary`, `ArchiveReader`, `schema.rs`, `auth.rs`, `auth.rs`, `object_output.rs`, `execute_object_import_batches`, `load_headless_config`, `ListTemplatesRequest`, `Result`, `discovery.rs`, `AnytypeGrpcClient`, `view.rs`, `validation.rs`, `handle`, `views.rs`, `chat_messages.rs`, `.backup_space`, `parse_filters`, `auth.rs`, `FileContentResponse`, `AnytypeError`, `error.rs`, `handle`, `crypto.rs`, `test_chat_stream.rs`, `handler_support.rs`, `MutationNumber`, `object_generator.rs`, `RestCollectionViewFilter`, `common.rs`, `fix_doc_list_indents`, `TestResultTracker`, `handle`, `AnyMcpServer`, `object_create.rs`, `HandlerError`, `init_tracing`, `verify.rs`, `main`, `main`, `logging.rs`, `.new`, `server.rs`, `config.rs`, `test_collect_all_matches_total`, `PageLimit`, `properties.rs`, `.listen_session_events`, `http_client.rs`, `IdempotencyKey`, `.new`, `DiscoveryReference`, `Result`, `.new`, `Result`, `FilesClient<'a>`, `protocol.rs`, `runtime.rs`, `Self`, `mutation_value.rs`, `execute_create`, `execute_object_import_batches`, `Result`?**
  _High betweenness centrality (0.625) - this node is a cross-community bridge._
- **Why does `CallToolResult` connect `discovery.rs` to `Type Models`, `.new`, `view_handlers.rs`, `stdio.rs`, `.new`, `object_edit.rs`, `Attempt`, `properties`, `headless_integration.rs`, `$defs`, `handler_support.rs`, `CancelledNotificationParams`, `protocol.rs`, `AnyMcpServer`, `object_create.rs`?**
  _High betweenness centrality (0.079) - this node is a cross-community bridge._
- **Why does `$defs` connect `$defs` to `properties`, `stdio.rs`, `properties`, `ElicitRequestFormParams`, `_meta`, `properties`, `properties`, `CancelledNotificationParams`, `properties`, `discovery.rs`, `properties`, `type`?**
  _High betweenness centrality (0.059) - this node is a cross-community bridge._
- **What connects `EDITOR_COMMAND`, `AuthCommand`, `EnabledToolset` to the rest of the system?**
  _686 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Chat Mock Server` be split into smaller, more focused modules?**
  _Cohesion score 0.07239819004524888 - nodes in this community are weakly interconnected._
- **Should `File Transfer API` be split into smaller, more focused modules?**
  _Cohesion score 0.006172839506172839 - nodes in this community are weakly interconnected._
- **Should `Space Pagination` be split into smaller, more focused modules?**
  _Cohesion score 0.06405856783344772 - nodes in this community are weakly interconnected._