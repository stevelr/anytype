# Graph Report - anyr-0.5  (2026-07-21)

## Corpus Check
- 191 files · ~520,715 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 8270 nodes · 25230 edges · 280 communities (256 shown, 24 thin omitted)
- Extraction: 97% EXTRACTED · 3% INFERRED · 0% AMBIGUOUS · INFERRED: 690 edges (avg confidence: 0.8)
- Token cost: 0 input · 0 output

## Graph Freshness
- Built from commit: `f50d921a`
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
- TestResult
- priority_groups.rs
- CancelledNotificationParams
- ResolveCandidate
- chat.rs
- ViewListObjectsRequest
- Vec
- view_handlers.rs
- mock.rs
- ProcessWatcher
- stdio_conformance.rs
- .new
- .create_template_fixtures
- Member
- result.rs
- Cli
- pagination_limit
- FilePreloadRequest
- enum
- main.rs
- String
- FileTypeArg
- spaces.rs
- resources.rs
- main
- route_aware_type_server
- ViewMatchAccumulator
- anyr
- ObjectSummary
- ArchiveReader
- ui.rs
- String
- list_command
- schema.rs
- TagColorArg
- Processor
- AuthArgs
- Self
- deserialize_vec_or_null
- Description
- auth.rs
- Key
- Platform
- auth.rs
- File
- SyncStatus
- HttpMetricsSnapshot
- object_output.rs
- .with_interceptor
- execute_object_import_batches
- .serialize
- MutationNumber
- main
- ListTemplatesRequest
- Result
- $defs
- ViewMatchAccumulator
- Account
- Widget
- test_rest_file_upload_download_and_delete
- discovery.rs
- EmailVerificationStatus
- InviteType
- AnytypeGrpcClient
- Text
- PeriodType
- PaginatedResponse<T>
- FixtureReply
- ChatSearchMessagesRequest
- .serialize
- view.rs
- TestAnyrCommands
- validation.rs
- filter_match
- EmailVerificationStatus
- InviteType
- Changelog
- handle
- PeriodType
- views.rs
- Platform
- State
- StatusType
- SyncStatus
- TemplateNamePrefillType
- chat_messages.rs
- .backup_space
- TimeFormat
- Option
- ensure_list_object
- Output
- parse_filters
- .create_collection_type_fixture
- .resolve_chat_target
- MutationProperty
- auth.rs
- FileContentResponse
- object_generator.rs
- AnytypeClient
- keys.rs
- .run
- ListSpacesRequest
- .create_space_fixture
- ChatMessage
- AnytypeError
- error.rs
- handle
- route_aware_type_server
- MutationNumber
- crypto.rs
- main
- ViewMatchAccumulator
- test_chat_stream.rs
- ListArchivedRequest
- main
- get_i64
- Condition
- DataSource
- DeviceNetworkType
- Key
- handler_support.rs
- Position
- Changelog
- SpaceShareableStatus
- Changelog
- SpaceType
- Style
- AuthArgs
- Setup
- anyback(1)
- .fmt
- Changelog
- FileDownloadRequest
- ImageResizeSchema
- fix_doc_list_indents
- TestResultTracker
- Message
- anyback
- AnyMcpServer
- object_create.rs
- init-cli-keys.sh
- Changelog
- Anytype gRPC client
- render_table
- verify.rs
- main
- main
- anytype-nonet
- logging.rs
- raycast-edit-anytype.sh
- server.rs
- test_collect_all_matches_total
- PageLimit
- properties.rs
- .listen_session_events
- Anytype Rust Tools and Clients
- prune-templates-keep-oldest.sh
- properties
- .new
- properties
- headless_integration.rs
- properties
- protocol.rs
- properties
- type
- mutation_value.rs
- execute_object_import_batches
- Result
- Attempt
- Result
- CompleteResult
- ProjectedDate
- .call_tool
- ElicitRequestFormParams
- jsonrpc
- properties
- run_smoke_tests
- CancelledNotificationParams
- load_headless_config
- Live-test mutation rate-limit audit
- README.md

## God Nodes (most connected - your core abstractions)
1. `Result` - 1468 edges
2. `String` - 1336 edges
3. `Request` - 422 edges
4. `Response` - 399 edges
5. `ClientCommandsClient<T>` - 343 edges
6. `Status` - 340 edges
7. `Error` - 132 edges
8. `with_test_context()` - 115 edges
9. `with_test_context_unit()` - 107 edges
10. `HttpClient` - 103 edges

## Surprising Connections (you probably didn't know these)
- `hex()` --references--> `String`  [EXTRACTED]
  any-mcp/src/cursor.rs → anytype-api/src/body.rs
- `verify_archive_state_with()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_archive.rs → anytype-api/src/verify.rs
- `execute_create()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_create.rs → anytype-api/src/verify.rs
- `object_edit()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_edit.rs → anytype-api/src/verify.rs
- `object_update()` --calls--> `verify_semantic()`  [INFERRED]
  any-mcp/src/object_update.rs → anytype-api/src/verify.rs

## Import Cycles
- None detected.

## Hyperedges (group relationships)
- **Authentication and Keystore Flow** — anytype_api_readme_anytype_api_client, anytype_api_keystores_interactive_authentication, anytype_api_keystores_authentication_token_storage, anytype_api_keystores_endpoint_specific_tokens [EXTRACTED 1.00]
- **gRPC Feature Surface** — anytype_api_changelog_grpc_backend, anytype_api_readme_grpc_api_extensions, anytype_api_readme_files_api, anytype_api_readme_chat_streaming, anytype_api_examples_readme_grpc_examples [EXTRACTED 1.00]

## Communities (280 total, 24 thin omitted)

### Community 0 - "Chat Mock Server"
Cohesion: 0.10
Nodes (18): DataModel, authorize_template_resource(), collection_fixture_ownership_error(), complete_type_object_id_snapshot(), current_space_create_response_never_enters_deletion_registry(), generic_pre_registered_id_cannot_claim_collection_provenance(), malformed_create_response_never_enters_deletion_registry(), registered_spaces() (+10 more)

### Community 1 - "File Transfer API"
Cohesion: 0.01
Nodes (334): AccountSelectTrace, AddFeatured, AddMessage, AddNotificationSubscriber, Ai, Align, AnyNameAllocate, AnyNameIsValid (+326 more)

### Community 2 - "Space Pagination"
Cohesion: 0.17
Nodes (22): build_yaml_front_matter(), detail_string(), detail_value_to_string(), extract_space_id_from_archive_name(), fixture_app(), fixture_entry(), focus_cycle_and_panel_jumps_work(), follow_link_and_back_navigation_restore_selection() (+14 more)

### Community 3 - "Integration Test Suite"
Cohesion: 0.04
Nodes (11): Block, ClientCommandsClient<T>, AppInfo, Placeholder, Request, Response, ChatMessage, Status (+3 more)

### Community 4 - "Authentication API"
Cohesion: 0.08
Nodes (122): add_to_list(), archive_file_paths(), archive_markdown_blob(), archive_object_ids(), archive_payload_file_paths(), assert_non_tty_output_clean(), backup_selected_ids(), ChatMessageTokenCleanupGuard (+114 more)

### Community 5 - "Chat Stream Builder"
Cohesion: 0.20
Nodes (12): Amount, Cart, CartProduct, CryptoCheckout, Data, Features, Invoice, Product (+4 more)

### Community 6 - "Filtering and Sorting"
Cohesion: 0.12
Nodes (35): cancellation_releases_permit_for_next_operation(), concurrency_limit_bounds_waiting_operations(), ControlledOperationError, default_control_failure_diagnostic(), eof_before_initialize_is_a_clean_shutdown(), execute_applies_end_to_end_timeout(), execute_honors_request_cancellation(), initialized_transport_shuts_down_cleanly_on_eof() (+27 more)

### Community 7 - "Object Models Utilities"
Cohesion: 0.11
Nodes (19): archived_object_from_search_result(), archived_relation_not_found(), archived_search_request(), CreateSpaceRequestBody, dataview_filter_type_in(), normalized_search_result_id(), prime_cache_spaces(), Icon (+11 more)

### Community 8 - "Pagination Core"
Cohesion: 0.07
Nodes (21): parse_message_attachment(), parse_message_attachments(), chat_message_path(), ChatAddMessageRequest, ChatEditMessageRequest, ChatGetMessageRequest, ChatHttpEditMessageRequest, grpc_attachments() (+13 more)

### Community 9 - "Client Configuration"
Cohesion: 0.06
Nodes (79): ArchiveObjectInfo, build_archive_object_index(), convert_archive_object_pb_to_markdown(), convert_archive_object_to_markdown(), convert_archive_snapshot_to_markdown(), convert_pb_json_snapshot_to_markdown(), convert_pb_snapshot_to_markdown(), convert_sample_pb_json_object_to_markdown_contains_headings() (+71 more)

### Community 10 - "Member Integration Tests"
Cohesion: 0.17
Nodes (12): description, properties, required, type, CancelledNotificationParams, reason, requestId, description (+4 more)

### Community 11 - "Type Request Models"
Cohesion: 0.15
Nodes (6): MockChatServer, MockChatServerHandle, Default, JoinHandle, Self, SocketAddr

### Community 12 - "Property Setter Tests"
Cohesion: 0.12
Nodes (10): BoundedText<MAX>, LastModified, LastModifiedError, ObjectResourceUri, AsRef, Cow, Deserialize, Display (+2 more)

### Community 13 - "Test Retry Helpers"
Cohesion: 0.05
Nodes (29): AnytypeClient, Color, CreateObjectRequestBody, Icon, ListObjectsRequest, NewObjectRequest, Object, object_link() (+21 more)

### Community 14 - "Client Cache"
Cohesion: 0.03
Nodes (90): Align, Amend, AutoArchive, AutoRestore, BackgroundColor, BlockCreate, BlockDuplicate, BlockMove (+82 more)

### Community 15 - "Process Watcher"
Cohesion: 0.16
Nodes (16): anytype_classifiers_cover_every_directly_constructible_error_variant(), anytype_error_mapping_discards_upstream_response_text(), AnytypeErrorMapping, api_error(), assert_anytype_mapping(), candidate_rich_anytype_ambiguity_maps_to_exact_tool_error(), mixed_anytype_candidates_retain_valid_alternatives(), mutation_http_rejection_allowlist_is_conservative_at_boundaries() (+8 more)

### Community 16 - "Changelog Concepts"
Cohesion: 0.12
Nodes (16): AnytypeClient, AtomicU64, Display, Duration, Formatter, Self, Semaphore, RuntimeContext (+8 more)

### Community 17 - "Chat Resolution Client"
Cohesion: 0.20
Nodes (7): authenticate(), broadcast_event(), build_chat_state(), build_event(), Future, Status, MetadataMap

### Community 18 - "Tag API"
Cohesion: 0.14
Nodes (21): AnytypeClient, chat_events_from_event(), chat_events_respect_sub_ids(), ChatEvent, ChatEventStream, ChatStreamControl, ChatStreamHandle, ChatStreamWorker (+13 more)

### Community 19 - "Property Value Models"
Cohesion: 0.07
Nodes (61): assert_public_mutation_sent_once(), assert_public_redirect_is_not_followed(), caller_supplied_reqwest_retry_policy_cannot_replay_a_mutation(), Capture, chunked_exact_limit_succeeds_without_content_length(), chunked_framing_cannot_bypass_limit_with_a_low_length_header(), content_length_exact_limit_succeeds(), DiagnosticChoice (+53 more)

### Community 20 - "Chat Message Models"
Cohesion: 0.15
Nodes (13): Authenticated stdio runtime, Document resources, Exact-match object edit workflow, Object archive workflow, Object create workflow, Object discovery and reads, Object update workflow, Phase 1 foundations (+5 more)

### Community 21 - "Member Models"
Cohesion: 0.11
Nodes (11): fmt_masked(), GrpcCredentials, HttpCredentials, KeyStoreType, Display, Formatter, Into, Option (+3 more)

### Community 22 - "View Models"
Cohesion: 0.33
Nodes (8): optional_body_schema(), optional_icon_schema(), optional_idempotency_schema(), optional_name_schema(), optional_properties_schema(), optional_template_schema(), Schema, SchemaGenerator

### Community 23 - "Identifier Resolution"
Cohesion: 0.04
Nodes (87): F, with_test_context_unit(), test_collect_all(), test_create_custom_property(), test_create_multiple_objects(), test_create_with_empty_name(), test_global_search(), test_invalid_object_id() (+79 more)

### Community 24 - "Property Request Builder"
Cohesion: 0.29
Nodes (6): Added, Added, Changed, Changed, Changelog, [Unreleased]

### Community 25 - "HTTP Retry Client"
Cohesion: 0.03
Nodes (84): archive_basename(), ArchiveCmpChanged, ArchiveCmpObject, ArchiveCmpReport, assert_backup_args_equal(), AuthArgs, AuthCommands, backup_export_options() (+76 more)

### Community 26 - "Property Lookup Helpers"
Cohesion: 0.07
Nodes (95): invalid_catalog_profile_fails_before_auth_without_echoing_its_value(), invalid_operational_setting_does_not_echo_its_value(), invalid_protocol_mode_fails_before_auth_without_echoing_its_value(), invalid_read_only_setting_fails_before_auth_without_echoing_its_value(), startup_auth_failure_is_nonzero_stderr_only_and_redacted(), unauthenticated_command(), add_to_list(), alpha_suffix() (+87 more)

### Community 27 - "Chat RPC Responses"
Cohesion: 0.04
Nodes (97): AddChatMessageResponse, append_sse_byte(), bool_field(), chat_message_from_grpc(), chat_state_from_grpc(), chat_stream_diagnostic_omits_url_credentials_query_and_fragment(), chat_stream_diagnostic_path(), ChatHttpEvent (+89 more)

### Community 28 - "Object Creation Builder"
Cohesion: 0.07
Nodes (17): ChatHttpAddMessageRequest, ChatHttpListMessagesRequest, ChatHttpReadMessagesRequest, ChatListMessagesRequest, ChatReadMessagesRequest, ChatReadReactionsRequest, ChatReadType, ChatSearchMessagesRequest (+9 more)

### Community 29 - "Search API"
Cohesion: 0.02
Nodes (49): AutofillMode, Change, ChangeNoSnapshot, Code, Content, Context, DetailsSet, DeviceAdd (+41 more)

### Community 30 - "Chat Attachments Reactions"
Cohesion: 0.11
Nodes (47): base_block(), bookmark_and_link_unknown_enum_values_read_opaque(), bookmark_link_relation_and_system_blocks_map(), callout_icon_prefers_image_over_emoji(), checkbox_checked_state_reads_verbatim(), cyclic_graphs_fail(), dangling_child_reference_fails(), duplicate_block_ids_fail() (+39 more)

### Community 31 - "Message Content Formatting"
Cohesion: 0.07
Nodes (26): AnytypeClient, CreateTypeProperty, CreateTypeRequestBody, deserialize_vec_properties_or_null(), ListTypesRequest, NewTypeRequest, prime_cache_types(), Arc (+18 more)

### Community 32 - "Input Validation"
Cohesion: 0.09
Nodes (24): AnytypeReference, FilterNumber, FilterNumberError, optional_body_input_schema(), optional_body_output_schema(), optional_cursor_schema(), optional_filter_schema(), optional_projection_schema() (+16 more)

### Community 33 - "Template API"
Cohesion: 0.08
Nodes (25): From, String, Condition, deserialize_vec_string_or_null(), Filter, FilterExpression, FilterOperator, join_values() (+17 more)

### Community 34 - "Type Models"
Cohesion: 0.06
Nodes (78): active_and_archived_scans_stop_at_explicit_page_and_item_bounds(), ambiguous_delete_failures_are_indeterminate_after_one_dispatch(), ambiguous_success_responses_recover_or_return_indeterminate_without_redelete(), archive_evidence(), archive_output(), archive_verification_config(), archive_verification_honors_hard_attempt_and_time_caps(), ArchivedState (+70 more)

### Community 35 - "Cache Controls"
Cohesion: 0.09
Nodes (25): QueryWithFilters, all_http_trace_levels_remain_metadata_only(), Arc<HttpClient>, deserialize_json(), HttpClient, HttpMetrics, HttpRequest, log_http_status() (+17 more)

### Community 36 - "Object Payload Models"
Cohesion: 0.67
Nodes (3): main(), Box, run()

### Community 37 - "Availability Verification"
Cohesion: 0.08
Nodes (32): Field, BlockParticipant, ChangePayload, DataviewRestrictions, DetailsSet, FileEncryptionKey, FileInfo, HistorySize (+24 more)

### Community 38 - "Object Accessors"
Cohesion: 0.20
Nodes (10): Accessibility Permissions, any-edit: Edit Anytype document in external editor, Build from source, Commands, Configure, Install, License, Platform compatibility (+2 more)

### Community 39 - "Chat Example CLI"
Cohesion: 0.20
Nodes (10): anyr, Build from source, Common options, Configure, Examples, Generating and saving credentials, Install, License (+2 more)

### Community 40 - "Object Update Examples"
Cohesion: 0.09
Nodes (63): archive_file_listing(), archive_signature(), AttachmentCaseBatch, BatchArtifacts, choose_writable_chat_space(), choose_writable_spaces(), cleanup_by_prefix(), cleanup_source_ids() (+55 more)

### Community 41 - "HTTP Request Methods"
Cohesion: 0.17
Nodes (25): block_id_and_color_token_validate_on_construction(), BlockContent, convert_block(), convert_color(), convert_content(), convert_embed(), convert_file(), convert_link() (+17 more)

### Community 42 - "object_edit.rs"
Cohesion: 0.07
Nodes (71): apply_edits(), bounded_result(), checked_space_id(), contract_is_closed_bounded_destructive_and_defaults_match_count(), edit_input(), edited_state_matches(), EditExecution, EditInputError (+63 more)

### Community 43 - "HTTP Metrics Reporting"
Cohesion: 0.07
Nodes (38): &'a mut PaginatedResponse<T>, &'a PagedResult<T>, &'a PaginatedResponse<T>, create_test_request(), next_response_iter(), PagedResult<T>, PaginatedResponse, PaginatedResponse<T> (+30 more)

### Community 44 - "object_edit.rs"
Cohesion: 0.25
Nodes (3): Self, Vec, TestResultTracker

### Community 45 - "Object Layout Tests"
Cohesion: 0.07
Nodes (49): Account, Add, BlockField, Cafe, ClientInfo, Config, DatabaseRecords, Details (+41 more)

### Community 46 - "object_edit.rs"
Cohesion: 0.25
Nodes (28): ChatAddMessageSvc, ChatDeleteMessageSvc, ChatEditMessageSvc, ChatGetMessagesByIdsSvc, ChatGetMessagesSvc, ChatReadAllSvc, ChatReadMessagesSvc, ChatSubscribeLastMessagesSvc (+20 more)

### Community 47 - "String"
Cohesion: 0.25
Nodes (4): Contributing to stevelr/anytype, Documentation, I Have a Question, Table of Contents

### Community 48 - "Error Types"
Cohesion: 0.25
Nodes (8): any-mcp, Build, Headless integration tests, License, Protocol channel, Quick start, Source layout, Testing

### Community 49 - "Object CRUD Requests"
Cohesion: 0.04
Nodes (23): Align, Block, BlockMetaOnly, CardStyle, Config, DataviewRestriction, EmptyType, FormulaType (+15 more)

### Community 50 - "Self"
Cohesion: 0.17
Nodes (6): file_type_filter(), file_type_from_mime(), FilePreloadRequest, FileType, grpc_file_type(), preload_source_tracks_url_and_path()

### Community 51 - "Chat Read State"
Cohesion: 0.14
Nodes (17): KeyStoreError, From, PathBuf, Self, VarError, default_platform_keyring(), init_keystore(), KeyStore (+9 more)

### Community 52 - "Object List Pagination"
Cohesion: 0.14
Nodes (21): ExportHeaderFormat, InviteArgs, InviteCommand, InviteCreateArgs, InviteRevokeArgs, InviteShowArgs, layout_filter(), ObjectArgs (+13 more)

### Community 53 - "Example Table Rendering"
Cohesion: 0.03
Nodes (36): ActionType, AttachmentType, Code, DateFormat, Description, EmailVerificationStatus, ErrorCode, FileIndexingStatus (+28 more)

### Community 54 - "Object"
Cohesion: 0.33
Nodes (6): Automated harness, Client configuration evidence, Current status, External tool evidence, Released compatibility matrix, Stdio protocol verification

### Community 55 - "String"
Cohesion: 0.17
Nodes (20): candidate_is_safe(), compare_candidates(), compare_duplicate_representatives(), insert_bounded_candidate(), malformed_template_resolution(), MatchAccumulator, MatchClassification, object_candidate() (+12 more)

### Community 56 - "Self"
Cohesion: 0.22
Nodes (19): ServeError, drain_frame(), encode_bounded_legacy_frame(), FirstFrame, FrameReadError, read_frame(), R, Receiver (+11 more)

### Community 57 - "Chat CRUD Tests"
Cohesion: 0.07
Nodes (21): AnytypeClient, chat_details_keys(), chat_search(), chat_search_space(), ChatClient<'a>, ChatCreateRequest, ChatDeleteMessageRequest, ChatGetMessagesRequest (+13 more)

### Community 58 - "Agenda Example"
Cohesion: 0.09
Nodes (52): backup_selected_ids(), BugDisposition, CaseStatus, choose_two_distinct_writable_spaces_cli(), choose_writable_space_cli(), clone_sqlite_with_sidecars(), cloned_test_keystore(), configure_test_keystore() (+44 more)

### Community 59 - "mod.rs"
Cohesion: 0.27
Nodes (9): chat_details(), number_value(), Option, Struct, Value, string_value(), value_bool(), value_number() (+1 more)

### Community 60 - "Value"
Cohesion: 0.09
Nodes (22): description, properties, type, description, items, type, items, Annotations (+14 more)

### Community 61 - "Value"
Cohesion: 0.25
Nodes (6): Checkbox, Date, Detail, Status, Tag, Value

### Community 62 - "File Example"
Cohesion: 0.10
Nodes (21): description, properties, required, type, description, properties, required, type (+13 more)

### Community 63 - "String"
Cohesion: 0.12
Nodes (12): file_from_http_upload(), FileDownloadDestination, FileHttpUploadRequest, FileSource, FileUploadRequest, FileUploadResponse, http_upload_file(), rest_upload_response_normalizes_to_file_object() (+4 more)

### Community 64 - "Basic Filters Example"
Cohesion: 0.11
Nodes (19): AnytypeCache, Arc, AsRef, Default, Formatter, HashMap, Mutex, Option (+11 more)

### Community 65 - "Interactive Auth Example"
Cohesion: 0.08
Nodes (19): AnytypeClient, ClientConfig, extract_port(), find_grpc(), lsof_listen_ports(), lsof_listen_ports_filters_prefix(), probe_grpc_port(), ResponseLimits (+11 more)

### Community 66 - "Value"
Cohesion: 0.11
Nodes (18): Bounded MCP filter DTO model (any-mcp), Condition mapping, Conversion strategy and consumption, Cursor binding rules, Error taxonomy, Excluded combinations and upstream limitations, Filter expression (group), Hard bounds (+10 more)

### Community 67 - "stdio.rs"
Cohesion: 0.14
Nodes (32): add_cache(), add_complete(), bounded_reader_recovers_at_the_next_line(), cancel_all(), decode(), dispatch_modern(), encode_result(), error_response() (+24 more)

### Community 68 - "Type Property Example"
Cohesion: 0.06
Nodes (61): clone_collection_view(), collection_fixture_transport_error(), collection_fixture_transport_error_redacts_tonic_status(), collection_matches_fixture_provenance(), collection_object(), collection_view_fixture_accepts_exact_new_event_identity(), collection_view_fixture_binds_object_show_root_and_exact_block(), collection_view_fixture_clone_changes_only_id_and_name() (+53 more)

### Community 69 - "stdio.rs"
Cohesion: 0.14
Nodes (25): application_profile_parser_is_exact_and_secret_safe(), config(), ConfigError, default_document_budget_is_routed_to_anytype_client(), defaults_are_bounded_and_reuse_anyr_keystore_service(), errors_name_the_variable_without_echoing_its_value(), maps_supported_anytype_environment_settings(), non_empty() (+17 more)

### Community 70 - "Consistency Retry Example"
Cohesion: 0.14
Nodes (10): App, clamp_scroll(), InputMode, LinkRow, PanelFocus, PathBuf, ObjectEntry, Picker (+2 more)

### Community 71 - "stdio.rs"
Cohesion: 0.07
Nodes (29): description, required, type, anyOf, description, $ref, description, required (+21 more)

### Community 72 - "Space Search Example"
Cohesion: 0.10
Nodes (49): unique_suffix(), with_test_context(), TestResult, test_body_read_preserves_typed_variants_ids_and_order(), test_body_read_reports_dataview_blocks_as_opaque(), is_expected_member_lookup_error(), TestResult, test_active_member_exists() (+41 more)

### Community 73 - "p1_cross_space.rs"
Cohesion: 0.17
Nodes (14): call_subscribe_last_messages(), get_messages_after(), AnytypeGrpcClient, Option, subscribe_previews(), unsubscribe_chat(), unsubscribe_previews(), ensure_error_ok() (+6 more)

### Community 74 - "index.rs"
Cohesion: 0.12
Nodes (35): build_preview(), build_preview_is_stable_and_compact(), build_preview_preserves_markdown_lines_without_truncating_headings(), collect_link_candidates(), collect_preview_strings(), collect_user_properties(), collect_user_properties_includes_array_and_object_values(), collect_user_properties_resolves_object_ids_to_name_and_id() (+27 more)

### Community 75 - "p1_cross_space.rs"
Cohesion: 0.13
Nodes (14): BoundedText, bounded_values(), ProjectedColor, ProjectedProperty, ProjectedTag, ProjectedValue, EntityId, Fn (+6 more)

### Community 76 - "String"
Cohesion: 0.15
Nodes (24): ambiguous_scans_also_fail_when_candidate_completeness_exceeds_the_limit(), candidate_membership_is_independent_of_input_order(), chat_id_with_space_passes_through(), classify_matches(), distinct_stable_ids_produce_deterministic_candidates(), duplicate_rows_for_one_stable_id_resolve_uniquely(), invalid_candidates_before_a_valid_match_do_not_hide_it(), match_classification_preserves_zero_and_one_items() (+16 more)

### Community 77 - ".new"
Cohesion: 0.16
Nodes (16): ChatClient, ChatHttpMessageStreamRequest, dropping_stream_cancels_incomplete_transport(), grpc_message_content(), mock_http_client(), one_transport_chunk_can_carry_multiple_exact_limit_events(), opening_transport_failure_discards_raw_url_and_source(), overflowing_stream_terminates_and_releases_transport_state() (+8 more)

### Community 78 - "with_test_context_unit"
Cohesion: 0.27
Nodes (9): ChatBackend, classify(), classify_messages(), OpTransport, resolve_transport(), Display, Formatter, Self (+1 more)

### Community 79 - "files.rs"
Cohesion: 0.17
Nodes (22): file_from_details(), FilesClient, FileStyle, filter_id_equal(), filter_to_dataview(), grpc_file_style(), grpc_filter_condition(), json_to_struct() (+14 more)

### Community 80 - "TestContext"
Cohesion: 0.14
Nodes (13): AnytypeClient, Arc, Filter, Into, IntoIterator, Object, Option, S (+5 more)

### Community 82 - "decode.rs"
Cohesion: 0.11
Nodes (46): build_expanded_entry_from_details(), derive_layout_name(), detail_value(), ExpandedSnapshotEntry, format_datetime_display(), format_datetime_with_tz(), format_last_modified(), format_utc_datetime_with_tz() (+38 more)

### Community 83 - "Option"
Cohesion: 0.13
Nodes (13): BlockId, BlockRef, BlockRestrictions, BodyBlock, BodyGraphErrorKind, BodySnapshot, ColorToken, graph_kind() (+5 more)

### Community 84 - "mod.rs"
Cohesion: 0.22
Nodes (8): choose_property_name(), handle(), handle_update(), property_command(), AppContext, Option, update_parses_name_and_key_forms(), validate_property_update()

### Community 85 - "create_object_with_retry"
Cohesion: 0.13
Nodes (39): create_object_with_retry(), ensure_properties_and_type(), is_key_already_exists_error(), lookup_property_tag_with_retry(), F, Object, Tag, TestResult (+31 more)

### Community 86 - "PaginatedResponse<T>"
Cohesion: 0.13
Nodes (18): CreateFingerprintV1, CreateName, CreateReference, fingerprint_hex(), FingerprintField, FingerprintField<'a, T>, IdempotencyKey, NormalizedCreate (+10 more)

### Community 87 - "find_list_object"
Cohesion: 0.16
Nodes (9): CreateInputError, FixtureReply, ObjectCreateHandlers, D, Display, Duration, Formatter, Into (+1 more)

### Community 88 - "NewTagRequest"
Cohesion: 0.07
Nodes (25): AnytypeClient, ListTagsRequest, NewTagRequest, Arc, Color, Filter, Into, Option (+17 more)

### Community 89 - "unique_test_name"
Cohesion: 0.13
Nodes (44): retry_definitive_rate_limit(), unique_test_name(), TestResult, test_create_custom_property(), test_create_property_duplicate_key(), test_create_property_invalid_name(), test_delete_property(), test_property_key_stability() (+36 more)

### Community 90 - "Widget"
Cohesion: 0.18
Nodes (11): metadata_table(), file_http_metadata(), file_path(), FileContentResponse, FileHttpMetadata, header_string(), insert_optional_header(), HeaderMap (+3 more)

### Community 91 - "QuickOption"
Cohesion: 0.11
Nodes (17): Client surface, Colors, alignment, backgrounds, Context and space ownership, Core model, Data sources and transport, Embeds, Error mapping, Graph validation (+9 more)

### Community 92 - "Style"
Cohesion: 0.21
Nodes (11): encode_legacy_message(), invalid_request(), is_jsonrpc_notification(), LegacyStdioTransport<R, W>, parse_error(), Future, Output, RoleServer (+3 more)

### Community 93 - "VerticalAlign"
Cohesion: 0.21
Nodes (12): must_have_body(), resolve_icon_exists(), AsRef, Icon, Into, Path, handle(), merge_properties() (+4 more)

### Community 94 - "TestResult"
Cohesion: 0.12
Nodes (6): DomainValueError, object_summary_serializes_with_canonical_resource_uri(), Into, validate_identifier(), returned_domain_error(), compare_domain_error()

### Community 95 - "priority_groups.rs"
Cohesion: 0.09
Nodes (40): assert_case_registered(), case_replace_after_object_type_changed_since_backup(), case_replace_object_type_collection_with_items(), case_replace_object_type_complex_nested_object(), case_replace_object_type_custom_type_object(), case_replace_object_type_file(), case_replace_object_type_object(), case_replace_object_type_property() (+32 more)

### Community 96 - "CancelledNotificationParams"
Cohesion: 0.10
Nodes (17): BookmarkContent, BookmarkState, byte_offset_at_utf16(), CalloutIcon, DividerStyle, HorizontalAlign, LayoutStyle, MarkKind (+9 more)

### Community 97 - "ResolveCandidate"
Cohesion: 0.36
Nodes (5): Display, Self, Tool, ServerBuildError, validate_catalog()

### Community 98 - "chat.rs"
Cohesion: 0.05
Nodes (43): backend_of(), blocks_json_rejected_with_transport_rest(), create_chat_object(), decode_order_id_arg(), decode_order_id_hex_roundtrip(), decode_order_id_non_hex_passthrough(), emit_message_rows(), encode_order_id_hex() (+35 more)

### Community 99 - "ViewListObjectsRequest"
Cohesion: 0.11
Nodes (22): AnytypeClient, deserialize_vec_filter_or_null(), deserialize_vec_sort_or_null(), fixture_client(), ListViewsRequest, Arc, D, Filter (+14 more)

### Community 100 - "Vec"
Cohesion: 0.06
Nodes (55): Auth, Bookmark, Chat, ChatState, Content, ContentValue, Div, Export (+47 more)

### Community 101 - "view_handlers.rs"
Cohesion: 0.07
Nodes (51): ObjectOutput, Page, ambiguous_view_name_returns_actionable_bounded_candidates(), convert_view_object_page(), convert_view_page(), fixture_client(), fixture_server(), object() (+43 more)

### Community 102 - "mock.rs"
Cohesion: 0.13
Nodes (21): chat_add_value(), chat_delete_value(), chat_state_update_value(), chat_update_value(), ChatRoom, filter_match(), filter_messages(), filters_match() (+13 more)

### Community 103 - "ProcessWatcher"
Cohesion: 0.13
Nodes (24): Account, matches_process_kind(), next_test_addr(), open_session_events(), ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatcher (+16 more)

### Community 104 - "stdio_conformance.rs"
Cohesion: 0.08
Nodes (51): assert_compact_wire_catalog(), assert_exact_decoder_error(), assert_exact_wire_catalog(), assert_exchange_depth(), assert_official_modern_request(), assert_official_modern_response(), assert_stdout_purity(), assert_structured_result() (+43 more)

### Community 105 - ".new"
Cohesion: 0.09
Nodes (22): additionalProperties, description, type, properties, description, properties, $ref, type (+14 more)

### Community 106 - ".create_template_fixtures"
Cohesion: 0.18
Nodes (21): cleanup_template_resource(), complete_global_template_owners(), complete_space_object_ids(), complete_template_ids(), complete_template_objects(), complete_template_ownership_snapshot(), complete_type_inventory(), delete_space_fixture() (+13 more)

### Community 107 - "Member"
Cohesion: 0.10
Nodes (16): AnytypeClient, ListMembersRequest, make_member(), Member, MemberRequest, MemberResponse, MemberRole, MemberStatus (+8 more)

### Community 109 - "Cli"
Cohesion: 0.15
Nodes (6): BackupExportFormat, BackupSpaceRequest, NewSpaceRequest, ExportFormat, PathBuf, Self

### Community 110 - "pagination_limit"
Cohesion: 0.08
Nodes (33): handle(), list_command(), list_objects_accepts_view(), list_objects_requires_view(), AppContext, ListArgs, handle(), AppContext (+25 more)

### Community 111 - "FilePreloadRequest"
Cohesion: 0.18
Nodes (10): chat_layout_filter(), ChatHttpListRequest, filter_id_equal(), filter_name_equal(), HttpChatEventEnvelope, request_json(), Filter, Value (+2 more)

### Community 112 - "enum"
Cohesion: 0.19
Nodes (16): AnytypeClient, BlocksClient, BlocksClient<'a>, body_fetch_applies_tightened_limits_over_grpc(), body_fetch_for_unknown_object_fails_without_closing(), body_fetch_round_trips_over_grpc_and_closes_the_view(), BodyLimits, BodyRequest (+8 more)

### Community 113 - "main.rs"
Cohesion: 0.15
Nodes (28): init_logging(), auth_login(), auth_logout(), AuthCommand, check_auth_status(), Cli, Commands, copy_link_command() (+20 more)

### Community 114 - "String"
Cohesion: 0.08
Nodes (57): ChatReadTypeArg, AppContext, AuthArgs, AuthCommands, build_client(), ChatArgs, ChatCommands, ChatMessagesArgs (+49 more)

### Community 115 - "FileTypeArg"
Cohesion: 0.36
Nodes (6): FileStyle, FileType, From, Self, FileStyleArg, FileTypeArg

### Community 116 - "spaces.rs"
Cohesion: 0.24
Nodes (10): FileObject, DateTime, FixedOffset, Sort, Vec, search_files(), sort_to_dataview(), PagedResult (+2 more)

### Community 117 - "resources.rs"
Cohesion: 0.09
Nodes (49): AnytypeResources, body_above_100k_chars_fails_without_silent_truncation(), cancellation_aborts_a_delayed_resource_read(), canonical_search_get_and_write_uri_type_round_trips_strictly(), controlled_error(), convert_object(), document_response_byte_ceiling_is_exact_before_conversion(), error_code() (+41 more)

### Community 118 - "main"
Cohesion: 0.17
Nodes (11): is_non_markdown_layout(), Path, Self, Terminal, run_editor_command(), sanitize_save_name(), shell_quote_path(), yaml_front_matter_includes_expected_fields() (+3 more)

### Community 119 - "route_aware_type_server"
Cohesion: 0.29
Nodes (7): LegacyStdioTransport, ListParams, JoinHandle, Option, PaginatedRequestParams, PhantomData, Sender

### Community 120 - "ViewMatchAccumulator"
Cohesion: 0.29
Nodes (5): candidate(), ErrorCandidate, EntityId, TryFrom, MAX_CANDIDATE_NAME_CHARS

### Community 121 - "anyr"
Cohesion: 0.25
Nodes (8): Before Submitting a Bug Report, Before Submitting an Enhancement, How Do I Submit a Good Bug Report?, How Do I Submit a Good Enhancement Suggestion?, I Want To Contribute, Reporting Bugs, Submitting PRs, Suggesting Enhancements

### Community 122 - "ObjectSummary"
Cohesion: 0.18
Nodes (7): ObjectSummary, D, DisplayName, ObjectId, Option, Self, SpaceId

### Community 123 - "ArchiveReader"
Cohesion: 0.13
Nodes (20): ArchiveFileEntry, ArchiveReader, ArchiveSourceKind, infer_object_id_from_snapshot_path(), infer_object_ids_from_files(), looks_like_content_id(), reader_lists_and_reads_directory_archive(), reader_lists_and_reads_zip_archive() (+12 more)

### Community 124 - "ui.rs"
Cohesion: 0.17
Nodes (31): buffer_to_lines(), draw(), draw_contents_panel(), draw_footer(), draw_help_overlay(), draw_links_panel(), draw_main_area(), draw_metadata_panel() (+23 more)

### Community 125 - "String"
Cohesion: 0.09
Nodes (16): column_widths(), FileObject, format_row(), format_separator(), Member, Object, Property, render_table() (+8 more)

### Community 126 - "list_command"
Cohesion: 0.38
Nodes (4): O, Tool, workflow_tool_attaches_strict_input_output_and_annotations(), WorkflowTool<O>

### Community 127 - "schema.rs"
Cohesion: 0.07
Nodes (53): BooleanTrueInput, BooleanTrueSchema, contains_keyword(), EmptyNestedInput, EmptySchema, FreeFormMapInput, FreeFormValueInput, has_only_keywords() (+45 more)

### Community 128 - "TagColorArg"
Cohesion: 0.27
Nodes (8): ambiguous(), AnytypeClient, not_found(), Into, Type, starts_with_uppercase(), type_and_property_key_resolution_bypass_cache_prime_paths(), looks_like_object_id()

### Community 129 - "Processor"
Cohesion: 0.33
Nodes (6): Color, Icon, MutationColor, MutationIcon, From, Icon

### Community 130 - "AuthArgs"
Cohesion: 0.22
Nodes (7): error_contains_stable_structured_and_compact_json_content(), ResultEncodingError, Display, Formatter, SummaryInput, SummaryResult, tool_error()

### Community 131 - "Self"
Cohesion: 0.12
Nodes (8): FileListRequest, FileSearchRequest, filter_not_empty(), Condition, Filter, IntoIterator, Self, size_filter()

### Community 132 - "deserialize_vec_or_null"
Cohesion: 0.20
Nodes (15): Attempt, BeginAttempt, CreateDisposition, CreateExecution, finish_supervised_execution(), IdempotencyStore, Arc, CancellationToken (+7 more)

### Community 133 - "Description"
Cohesion: 0.31
Nodes (7): definitive_rate_limit_retry_delay(), Duration, api_error(), definitive_rate_limit_backoff_is_finite_and_bounded(), definitive_rate_limit_retry_exhausts_at_the_finite_attempt_cap(), indeterminate_failure_is_never_retried(), only_typed_http_429_is_a_definitive_retryable_rejection()

### Community 134 - "auth.rs"
Cohesion: 0.22
Nodes (10): AuthStatus, CreateApiKeyRequest, CreateApiKeyResponse, CreateChallengeRequest, CreateChallengeResponse, GrpcStatus, HttpStatus, KeyStoreStatus (+2 more)

### Community 135 - "Key"
Cohesion: 0.18
Nodes (10): ChatService, EventStream, Box, Pin, Poll, Receiver, Stream, Body (+2 more)

### Community 136 - "Platform"
Cohesion: 0.16
Nodes (8): diagnostic_path(), diagnostic_path_keeps_only_bounded_non_control_path_context(), has_invalid_percent_encoding(), HttpMetricsSnapshot, parse_diagnostic_target(), Display, Formatter, Option

### Community 137 - "auth.rs"
Cohesion: 0.12
Nodes (30): create_local_link_challenge(), create_session(), create_session_token(), create_session_token_from_account_key(), create_session_token_from_app_key(), LocalLinkCredentials, AsRef, Channel (+22 more)

### Community 138 - "File"
Cohesion: 0.11
Nodes (16): Bookmark, Description, FaviconHash, File, Hash, ImageHash, Mime, Name (+8 more)

### Community 139 - "SyncStatus"
Cohesion: 0.14
Nodes (13): ambiguity_error_exposes_only_bounded_candidates(), candidate_selection_filters_invalid_values_before_its_cap(), domain_candidates_deduplicate_space_type_view_chat_and_property_rows(), fixture_client_with_grpc(), later_direct_view_id_wins_over_earlier_name_ambiguity(), MatchAccumulator<T>, property_candidate(), public_chat_resolution_composes_bounded_http_and_grpc_discovery() (+5 more)

### Community 140 - "HttpMetricsSnapshot"
Cohesion: 0.13
Nodes (10): MutationDate, MutationIconText, MutationPropertyKey, Cow, Deserialize, JsonSchema, Schema, SchemaGenerator (+2 more)

### Community 141 - "object_output.rs"
Cohesion: 0.15
Nodes (31): all_properties(), all_property_variants_have_closed_bounded_wire_forms(), bounded(), bounded_date(), cursor_projection_normalization_is_order_insensitive(), last_modified(), malformed_oversized_and_duplicate_selected_values_fail_closed(), metadata_domain() (+23 more)

### Community 142 - ".with_interceptor"
Cohesion: 0.33
Nodes (4): F, T, InterceptedService, Uri

### Community 143 - "execute_object_import_batches"
Cohesion: 0.10
Nodes (35): ObjectDescriptor, apply_import_response(), BackupSelection, build_import_plan(), build_import_plan_infers_ids_without_manifest_from_directory(), build_import_plan_uses_archive_path_directly(), descriptor_matches_type_filter(), descriptors_from_selection() (+27 more)

### Community 145 - "MutationNumber"
Cohesion: 0.25
Nodes (5): AnytypeClient, delete_archived_best_effort(), ListArchivedRequest<'a>, AsRef, run_archived_search()

### Community 146 - "main"
Cohesion: 0.50
Nodes (4): find_list_object(), main(), Object, Option

### Community 147 - "ListTemplatesRequest"
Cohesion: 0.18
Nodes (11): AnytypeClient, ListTemplatesRequest, Arc, Filter, Into, Object, Option, Self (+3 more)

### Community 148 - "Result"
Cohesion: 0.14
Nodes (47): exit_code(), T, with_token(), auth_status(), AuthCommand, AuthSource, AuthStatusArgs, build_yaml_export() (+39 more)

### Community 149 - "$defs"
Cohesion: 0.05
Nodes (50): description, properties, type, description, properties, type, description, properties (+42 more)

### Community 150 - "ViewMatchAccumulator"
Cohesion: 0.67
Nodes (3): TestResult, test_collect_all_matches_total(), test_stream_matches_collect_all()

### Community 151 - "Account"
Cohesion: 0.18
Nodes (8): BackoffPolicy, ChatStreamBuilder, AnytypeClient, Default, Duration, Pin, Poll, Self

### Community 152 - "Widget"
Cohesion: 0.50
Nodes (4): Limit, ViewId, Widget, Layout

### Community 154 - "discovery.rs"
Cohesion: 0.04
Nodes (116): all_discovery_contracts_are_strict_bounded_read_tools(), assert_error(), checked_tag_count(), concise_summaries_preserve_closed_fields_only(), convert_property_summary(), convert_space_summary(), convert_tag_summary(), convert_type_summary() (+108 more)

### Community 155 - "EmailVerificationStatus"
Cohesion: 0.40
Nodes (3): Account, Info, Status

### Community 156 - "InviteType"
Cohesion: 0.22
Nodes (15): HandlerOperationError, Display, From, execute_create(), indeterminate_operation(), EntityId, Object, ObjectId (+7 more)

### Community 157 - "AnytypeGrpcClient"
Cohesion: 0.18
Nodes (9): AnytypeGrpcClient, AnytypeGrpcConfig, default_grpc_endpoint(), AsRef, Channel, Default, Into, Self (+1 more)

### Community 158 - "Text"
Cohesion: 0.11
Nodes (19): Checked, Color, Div, IconEmoji, IconImage, Latex, Link, Marks (+11 more)

### Community 159 - "PeriodType"
Cohesion: 0.38
Nodes (5): Formatter, ColorArg, init_tracing(), main(), run()

### Community 160 - "PaginatedResponse<T>"
Cohesion: 0.23
Nodes (10): canonical_returned_date(), MutationCompareError, returned_id(), returned_ids(), returned_tag_ids(), Formatter, Option, Tag (+2 more)

### Community 161 - "FixtureReply"
Cohesion: 0.33
Nodes (5): ListenSessionEventsSvc, AtomicU64, Sender, StreamRequest, ServerStreamingService

### Community 162 - "ChatSearchMessagesRequest"
Cohesion: 0.67
Nodes (3): TestResult, test_chat_discovery_requests(), test_rest_chat_messages_reactions_search_and_reads()

### Community 163 - ".serialize"
Cohesion: 0.29
Nodes (5): ambiguity_candidates_are_bounded(), AmbiguityCandidatesError, Display, Formatter, IntoIterator

### Community 164 - "view.rs"
Cohesion: 0.17
Nodes (27): build_member_identity_map(), load_member_cache(), MemberCache, parse_member_identity(), resolve_member_name(), AppContext, HashMap, Option (+19 more)

### Community 165 - "TestAnyrCommands"
Cohesion: 0.17
Nodes (7): anyr_bin(), base_env(), run_anyr(), run_anyr_json(), run_help(), TestAnyrCommands, CompletedProcess

### Community 166 - "validation.rs"
Cohesion: 0.09
Nodes (27): body_limits_and_exact_unicode_boundaries(), BodyCharLimit, BodyChunk, BodyChunkInput, BodyOffset, BoundedList, BoundedList<T, MAX>, chunk_body() (+19 more)

### Community 167 - "filter_match"
Cohesion: 0.67
Nodes (3): TestResult, test_chat_message_crud(), test_rest_chat_message_crud()

### Community 168 - "EmailVerificationStatus"
Cohesion: 0.67
Nodes (3): deserialize_vec_or_null(), D, T

### Community 170 - "Changelog"
Cohesion: 0.09
Nodes (21): [0.2.2] - anyr - 2026-01-12, [0.2.3] - anyr - 2026-01-12, [0.2.4] - anyr - 2026-01-17, [0.3.0] - anyr - 2026-01-28, [0.4.0] - anyr - 2026-02-16, [0.4.1], Added, Added (+13 more)

### Community 171 - "handle"
Cohesion: 0.06
Nodes (34): apply_file_filters_list(), apply_file_filters_search(), delete_accepts_permanent(), delete_defaults_to_non_permanent(), discard_preload_parses_space_and_file_id(), download_http(), download_parses_space_object_and_rest_options(), file_command() (+26 more)

### Community 172 - "PeriodType"
Cohesion: 0.28
Nodes (14): explicit_type_id_resolution_rejects_safe_mismatched_identity(), fixture_client(), paged_fixture_server(), public_space_resolution_deduplicates_across_http_pages(), public_view_resolution_preserves_later_direct_id_across_pages(), Value, template_direct_id_uses_one_get_and_revalidates_identity(), template_page() (+6 more)

### Community 173 - "views.rs"
Cohesion: 0.16
Nodes (20): BlockDataview, ClientCommandsClient, authenticated_request(), fetch_grid_view_columns(), find_dataview_block(), GridViewColumn, GridViewInfo, invalid_view_token_is_preserved_as_typed_auth_error() (+12 more)

### Community 174 - "Platform"
Cohesion: 0.33
Nodes (5): MutationInputError, D, Display, Into, Self

### Community 176 - "StatusType"
Cohesion: 0.19
Nodes (9): parse_message_mark(), parse_message_marks(), Vec, ChatEditTextRequest, ChatSendTextRequest, http_message_style(), MessageTextMark, MessageTextStyle (+1 more)

### Community 178 - "TemplateNamePrefillType"
Cohesion: 0.13
Nodes (15): Groups, Dataview, Filter, Group, Block, FileInfo, Filter, GroupOrder (+7 more)

### Community 179 - "chat_messages.rs"
Cohesion: 0.21
Nodes (15): Cli, Commands, format_order_id(), hex_to_bytes(), hex_value(), is_hex(), last_five_chars(), list_chats() (+7 more)

### Community 180 - ".backup_space"
Cohesion: 0.20
Nodes (11): AnytypeGrpcClient, generated_target_name(), ExportFormat, Into, PathBuf, Self, Vec, sanitize_path_component() (+3 more)

### Community 181 - "TimeFormat"
Cohesion: 0.22
Nodes (4): Arc, Into, SpaceRequest, UpdateSpaceRequest

### Community 182 - "Option"
Cohesion: 0.29
Nodes (7): CachedObject, epoch_to_rfc3339(), ObjectCache, Option, Vec, value_as_rfc3339(), LruCache

### Community 183 - "ensure_list_object"
Cohesion: 0.34
Nodes (15): ObjectLayout, ensure_list_object(), find_list_object_by_layout(), list_views_with_retry(), Object, Option, TestResult, Vec (+7 more)

### Community 184 - "Output"
Cohesion: 0.29
Nodes (6): Output, OutputFormat, Option, PathBuf, Self, T

### Community 185 - "parse_filters"
Cohesion: 0.27
Nodes (11): mode_clear_when_clear_flag(), mode_merge_when_add_properties(), mode_replace_parses_set_properties(), mode_unchanged_when_no_flags(), Vec, type_command(), type_property_mode(), TypePropertyMode (+3 more)

### Community 186 - ".create_collection_type_fixture"
Cohesion: 0.15
Nodes (11): collection_type_details(), collection_type_fixture_details_use_the_canonical_heart_layout(), CompleteTypeInventory, Into, Struct, Type, Value, Vec (+3 more)

### Community 187 - ".resolve_chat_target"
Cohesion: 0.26
Nodes (5): bare_chat_id_discovery_has_one_global_space_and_chat_budget(), ChatTarget, resolution_limit(), ResolutionScanBudget, Option

### Community 188 - "MutationProperty"
Cohesion: 0.25
Nodes (7): generic_set_property_application_omits_format_and_preserves_canonical_values(), ids(), MutationIds, MutationProperty, EntityId, R, MutationText

### Community 189 - "auth.rs"
Cohesion: 0.18
Nodes (19): credentials_summary(), credentials_summary_distinguishes_missing_grpc(), credentials_summary_distinguishes_missing_http(), credentials_summary_marks_both_missing(), credentials_summary_marks_both_present(), find_grpc_cmd(), handle(), HeadlessConfig (+11 more)

### Community 190 - "FileContentResponse"
Cohesion: 0.13
Nodes (13): AnytypeClient, conditional_not_modified_status_is_preserved(), FileContentRequest, FileDeleteRequest, FileDiscardPreloadRequest, FileGetRequest, FilesClient<'a>, head_returns_file_metadata_without_a_body() (+5 more)

### Community 191 - "object_generator.rs"
Cohesion: 0.31
Nodes (10): cleanup_by_ids(), cleanup_by_name_prefix(), create_object_once(), create_object_with_retry(), generate_fixture(), GeneratedFixture, GeneratedObject, AnytypeClient (+2 more)

### Community 192 - "AnytypeClient"
Cohesion: 0.27
Nodes (3): AnytypeClient, F, Into

### Community 193 - "keys.rs"
Cohesion: 0.25
Nodes (3): KeyAction, map_key_with_input_mode(), KeyEvent

### Community 194 - ".run"
Cohesion: 0.31
Nodes (3): GrpcSession, open_session_events(), Streaming

### Community 195 - "ListSpacesRequest"
Cohesion: 0.28
Nodes (4): dataview_filter_checkbox_equal(), ListSpacesRequest, Filter, IntoIterator

### Community 196 - ".create_space_fixture"
Cohesion: 0.38
Nodes (6): complete_space_id_snapshot(), Space, space_fixture_ownership_error(), space_listing_evidence(), space_page_is_complete(), SpaceListingEvidence

### Community 197 - "ChatMessage"
Cohesion: 0.29
Nodes (7): ChatMessage, IdentityList, Reactions, HashMap, MessageBlock, MessageContent, UpdateReactions

### Community 198 - "AnytypeError"
Cohesion: 0.04
Nodes (32): main(), main(), main(), main(), main(), main(), main(), main() (+24 more)

### Community 199 - "error.rs"
Cohesion: 0.21
Nodes (5): Self, Vec, ToolError, ToolErrorCode, upstream_error_is_stable_and_contains_no_diagnostic_input()

### Community 200 - "handle"
Cohesion: 0.31
Nodes (13): handle(), HeadlessConfig, login(), logout(), AppContext, AuthArgs, Option, PathBuf (+5 more)

### Community 201 - "route_aware_type_server"
Cohesion: 0.33
Nodes (6): empty_page_server(), explicit_type_id_resolution_uses_one_direct_get_with_cache_enabled(), route_aware_type_server(), JoinHandle, Sender, TypeRouteTraffic

### Community 202 - "MutationNumber"
Cohesion: 0.70
Nodes (3): canonical_returned_number(), MutationNumber, Number

### Community 203 - "crypto.rs"
Cohesion: 0.35
Nodes (10): crc16_xmodem(), derive_keys_from_mnemonic(), derive_keys_from_mnemonic_go_vector(), encode_account_id(), slip10_derive_child(), slip10_derive_master(), slip10_derive_path(), slip10_derive_path_matches_stepwise() (+2 more)

### Community 204 - "main"
Cohesion: 0.50
Nodes (4): main(), MessageContent, Object, status_color()

### Community 205 - "ViewMatchAccumulator"
Cohesion: 0.70
Nodes (3): View, view_candidate(), ViewMatchAccumulator

### Community 206 - "test_chat_stream.rs"
Cohesion: 0.31
Nodes (9): chat_stream_receives_messages(), chat_stream_reconnects_after_disconnect(), rest_chat_stream_receives_initial_message(), AnytypeClient, F, SocketAddr, setup_mock_client(), wait_for_event() (+1 more)

### Community 207 - "ListArchivedRequest"
Cohesion: 0.40
Nodes (5): DeleteAllArchivedResult, DeleteBestEffortResult, ListArchivedRequest, AnytypeClient, Vec

### Community 208 - "main"
Cohesion: 0.40
Nodes (3): main(), Box, LocalApiScope

### Community 209 - "get_i64"
Cohesion: 0.50
Nodes (4): get_i64(), int_list(), BTreeMap, Value

### Community 214 - "handler_support.rs"
Cohesion: 0.06
Nodes (66): assert_error(), begin_page(), BoundedResult, continuation_advances_by_upstream_window_not_sparse_item_count(), contract(), ControlledFailurePolicy, conversion_and_encoding_failures_emit_one_safe_failure_diagnostic(), cursor_binding_includes_tool_limit_and_normalized_non_cursor_params() (+58 more)

### Community 216 - "Changelog"
Cohesion: 0.15
Nodes (12): [0.1.1] - any-edit, [0.1.2] - any-edit - 2026-01-17, [0.1.3] - any-edit - 2026-01-28, [0.1.5], Added, Changed, Changed, Changed (+4 more)

### Community 218 - "Changelog"
Cohesion: 0.15
Nodes (12): [0.2.0] - anytype-rpc - 2026-01-17, [0.2.1] - anytype-rpc - 2026-01-28, [0.3.0] - anytype-rpc - 2026-02-16, [0.3.1], Added, Added, Added, Changed (+4 more)

### Community 222 - "Setup"
Cohesion: 0.17
Nodes (11): 1) Authenticate, 2) Configure the script, 3) Add the script to Raycast, 4) Assign a hotkey, 5) Grant Accessibility permissions (macOS), Common issues, Diagnostics, Raycast setup and diagnostics (+3 more)

### Community 223 - "anyback(1)"
Cohesion: 0.17
Nodes (11): anyback(1), BACKUP OUTPUT, DESCRIPTION, ENVIRONMENT VARIABLES, EXIT STATUS, EXTRACT, GLOBAL OPTIONS, NAME (+3 more)

### Community 225 - "Changelog"
Cohesion: 0.06
Nodes (41): Ambiguous Resolution Error, Archived Object Management, Changelog, DB Keystore Migration, gRPC Backend, Process Watcher, Resolve Module, Semantic Versioning (+33 more)

### Community 227 - "FileDownloadRequest"
Cohesion: 0.29
Nodes (5): FileDownloadRequest, rich_and_url_uploads_select_grpc(), AsRef, Path, simple_path_and_byte_uploads_select_rest()

### Community 228 - "ImageResizeSchema"
Cohesion: 0.33
Nodes (10): FileInfo, FileKeys, ImageResizeSchema, Link, HashMap, Link, Option, Struct (+2 more)

### Community 235 - "fix_doc_list_indents"
Cohesion: 0.46
Nodes (7): fix_doc_list_indents(), indent_doc_list_continuation(), indent_doc_list_line(), main(), Box, Option, PathBuf

### Community 239 - "TestResultTracker"
Cohesion: 0.06
Nodes (34): cursor_binding_tampering_and_process_expiry(), cursor_parts(), cursor_registry_evicts_oldest_state_at_its_cap(), CursorStore, CursorStoreError, CursorToken, hex(), hex_nibble() (+26 more)

### Community 241 - "Message"
Cohesion: 0.20
Nodes (10): BlockUpdate, DropFiles, Export, Import, Message, Migration, PreloadFile, ResponseEvent (+2 more)

### Community 249 - "anyback"
Cohesion: 0.22
Nodes (8): anyback, Commands, Development, Features, Integrity Testing, Library Crate, Restore Transport, Usage Notes

### Community 251 - "AnyMcpServer"
Cohesion: 0.14
Nodes (20): AnyMcpServer, decode_arguments(), invalid_arguments(), reject_static_cursor(), Arc, CancellationToken, ErrorData, JsonObject (+12 more)

### Community 253 - "object_create.rs"
Cohesion: 0.25
Nodes (28): all_shared_property_and_icon_forms_reach_one_canonical_create_payload(), first_postdispatch_failures_are_fixed_and_key_retry_never_posts_twice(), fixture(), identical_sequential_and_concurrent_keyed_calls_create_once(), input(), input_with_body(), mismatched_key_reuse_and_read_only_cached_call_do_no_io(), named_space_type_and_template_are_bounded_and_revalidated_before_create() (+20 more)

### Community 257 - "init-cli-keys.sh"
Cohesion: 0.32
Nodes (5): ANYTYPE_GRPC_ENDPOINT, ANYTYPE_URL, init_cli_and_keystore(), join_space(), init-cli-keys.sh script

### Community 260 - "Changelog"
Cohesion: 0.29
Nodes (6): 0.1.0 - 2026-02-10, [0.3.0 - alpha] - anyback - 2026-02-16, [0.4.0-alpha.2], Changed, Changelog, [Unreleased]

### Community 263 - "Anytype gRPC client"
Cohesion: 0.29
Nodes (6): Anytype gRPC client, Building, Compatibility, License, Related projects, Status and plan

### Community 272 - "render_table"
Cohesion: 0.60
Nodes (4): format_row(), format_separator(), render_table(), Vec

### Community 274 - "verify.rs"
Cohesion: 0.09
Nodes (34): set_property_tags(), refresh_cached_property_tags(), api_error(), availability_wrapper_preserves_first_success_behavior(), config(), dropped_verifier_drops_in_flight_fetch_without_retaining_value(), exact_attempt_cap_is_enforced_for_zero_delay(), legacy_availability_retry_classifier_preserves_exact_parity() (+26 more)

### Community 289 - "logging.rs"
Cohesion: 0.09
Nodes (29): build_filter(), Capture, capture_default(), dependency_payload_targets_cannot_override_metadata_deny_filter(), ensure_trace_interest(), has_target_prefix(), init(), LoggingError (+21 more)

### Community 295 - "raycast-edit-anytype.sh"
Cohesion: 0.67
Nodes (3): EDITOR_COMMAND, notify(), raycast-edit-anytype.sh script

### Community 301 - "server.rs"
Cohesion: 0.10
Nodes (44): ApplicationProfile, assert_catalog_contracts(), assert_valid_representative(), audit_schema(), canonical_json(), capabilities_are_static_complete_and_never_advertise_list_changed(), catalog_entries_equal_the_original_typed_contracts(), catalog_snapshot() (+36 more)

### Community 314 - "test_collect_all_matches_total"
Cohesion: 0.06
Nodes (73): TypeKey, HandlerError, array_condition(), ArrayCondition, checkbox_and_number_filters_are_forwarded_without_rewriting(), checkbox_condition(), CheckboxCondition, chunked_get_hashes_complete_unicode_body_without_leaking_remainder() (+65 more)

### Community 315 - "PageLimit"
Cohesion: 0.09
Nodes (18): PageRequest, non_null_cursor_schema(), page_omits_terminal_cursor_from_json(), Page<T>, PageLimit, PageOffset, PaginationInput, Cow (+10 more)

### Community 322 - "properties.rs"
Cohesion: 0.04
Nodes (58): property(), AnytypeClient, assert_malformed_tag_pagination(), CreatePropertyRequestBody, deserialize_vec_string_or_null(), deserialize_vec_tag_or_null(), direct_property_get_is_cache_independent_and_exactly_scoped(), direct_property_get_validates_both_ids_before_io() (+50 more)

### Community 377 - "properties"
Cohesion: 0.07
Nodes (32): description, properties, type, description, enum, type, description, $ref (+24 more)

### Community 387 - ".new"
Cohesion: 0.06
Nodes (96): BodySha256, checked_effective_type(), checked_space_id(), checked_type_key(), closed_property_type_and_icon_values_are_sent_and_verified(), contract_is_destructive_closed_and_null_is_never_omission(), definitive_patch_4xx_is_ordinary_and_skips_verification(), documented_empty_forms_clear_body_and_clearable_properties() (+88 more)

### Community 388 - "properties"
Cohesion: 0.06
Nodes (31): description, properties, required, type, CreateMessageRequestParams, description, enum, type (+23 more)

### Community 401 - "headless_integration.rs"
Cohesion: 0.24
Nodes (25): active_contains(), archived_contains(), arguments(), assert_archive_evidence(), assert_collection_view_continuation(), assert_cursor_continuation(), assert_fixture_space_continuation(), assert_fixture_template_continuation() (+17 more)

### Community 403 - "properties"
Cohesion: 0.12
Nodes (18): required, required, description, required, type, CreateMessageResult, DiscoverResult, description (+10 more)

### Community 441 - "protocol.rs"
Cohesion: 0.23
Nodes (8): Input, Output, ObjectId, PhantomData, ToolProfile, UnboundedOutput, workflow_tool(), ToolAnnotations

### Community 443 - "properties"
Cohesion: 0.08
Nodes (25): $ref, description, properties, type, ClientCapabilities, description, properties, type (+17 more)

### Community 444 - "type"
Cohesion: 0.09
Nodes (25): properties, required, type, type, BooleanSchema, type, additionalProperties, default (+17 more)

### Community 455 - "mutation_value.rs"
Cohesion: 0.13
Nodes (16): comparison_classifies_bounded_and_malformed_upstream_values(), dates_are_bounded_rfc3339_and_canonical_utc(), ids_cap_raw_input_before_sorting_and_deduplication(), MutationContractInput, normalized_properties(), normalized_values_serialize_deterministically_for_future_fingerprints(), only_documented_empty_values_compare_as_missing_clears(), properties_are_bounded_sorted_and_duplicate_keys_reject() (+8 more)

### Community 477 - "execute_object_import_batches"
Cohesion: 0.09
Nodes (39): aggregate_import_responses(), AppContext, build_import_plan_infers_ids_without_manifest_from_zip(), dir_contains_pb_or_json(), execute_object_import(), execute_object_import_batches(), execute_object_import_path(), finalize_backup_output_path() (+31 more)

### Community 478 - "Result"
Cohesion: 0.03
Nodes (15): Formatter, Formatter, Path, run_inspector(), init_tracing(), main(), run(), main() (+7 more)

### Community 491 - "Attempt"
Cohesion: 0.17
Nodes (18): all_property_object_value(), all_property_type_value(), contract_is_strict_bounded_non_null_and_uses_create_annotations(), fingerprint_v1_is_domain_separated_golden_and_semantically_canonical(), is_canonical_plain_body(), is_plain_body_line(), normalize_create_body(), object_create_tool() (+10 more)

### Community 493 - "Result"
Cohesion: 0.28
Nodes (8): numeric_boundary(), HashMap, HashSet, Map, Value, SchemaAudit, SchemaAudit<'root>, validate_finite_scalar()

### Community 518 - "CompleteResult"
Cohesion: 0.09
Nodes (22): description, properties, required, type, properties, required, type, CompleteResult (+14 more)

### Community 534 - "ProjectedDate"
Cohesion: 0.16
Nodes (11): ProjectedDate, ProjectedNumber, Cow, D, Deserialize, Into, JsonSchema, Schema (+3 more)

### Community 552 - ".call_tool"
Cohesion: 0.10
Nodes (19): Any, CancellationToolServer, Arc, Future, Notify, Option, Output, RequestContext (+11 more)

### Community 553 - "ElicitRequestFormParams"
Cohesion: 0.12
Nodes (17): ElicitRequestFormParams, ElicitRequestURLParams, description, properties, required, type, description, properties (+9 more)

### Community 585 - "jsonrpc"
Cohesion: 0.30
Nodes (15): required, required, required, required, required, required, required, required (+7 more)

### Community 595 - "properties"
Cohesion: 0.07
Nodes (30): $ref, description, properties, required, type, description, format, type (+22 more)

### Community 597 - "run_smoke_tests"
Cohesion: 0.17
Nodes (16): Instant, Iter, TestContext, TestResults, run_smoke_tests(), smoke_test(), test_filters(), test_members_api() (+8 more)

### Community 631 - "CancelledNotificationParams"
Cohesion: 0.09
Nodes (24): properties, properties, anyOf, description, type, properties, description, type (+16 more)

### Community 655 - "load_headless_config"
Cohesion: 0.46
Nodes (7): AnytypeHeadlessConfig, default_headless_config_path(), load_headless_config(), ConfigError, Option, Path, PathBuf

### Community 774 - "Live-test mutation rate-limit audit"
Cohesion: 0.50
Nodes (3): Deliberate exclusions, Live-test mutation rate-limit audit, Retried setup inventory

## Knowledge Gaps
- **716 isolated node(s):** `EDITOR_COMMAND`, `AuthCommand`, `EnabledToolset`, `EmptyPageParams`, `SpacePageParams` (+711 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **24 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `String` connect `Template API` to `Chat Mock Server`, `File Transfer API`, `Space Pagination`, `Integration Test Suite`, `Authentication API`, `Chat Stream Builder`, `Object Models Utilities`, `Pagination Core`, `Client Configuration`, `Property Setter Tests`, `Test Retry Helpers`, `Client Cache`, `Chat Resolution Client`, `Tag API`, `Property Value Models`, `Member Models`, `ProjectedDate`, `HTTP Retry Client`, `Property Lookup Helpers`, `Chat RPC Responses`, `Object Creation Builder`, `Search API`, `Chat Attachments Reactions`, `Message Content Formatting`, `Input Validation`, `Type Models`, `Cache Controls`, `Availability Verification`, `Object Update Examples`, `HTTP Request Methods`, `object_edit.rs`, `HTTP Metrics Reporting`, `object_edit.rs`, `Object Layout Tests`, `object_edit.rs`, `Object CRUD Requests`, `Self`, `Chat Read State`, `Object List Pagination`, `Example Table Rendering`, `String`, `Chat CRUD Tests`, `Agenda Example`, `mod.rs`, `Value`, `String`, `Basic Filters Example`, `Interactive Auth Example`, `stdio.rs`, `Type Property Example`, `stdio.rs`, `Consistency Retry Example`, `Space Search Example`, `p1_cross_space.rs`, `index.rs`, `p1_cross_space.rs`, `String`, `.new`, `files.rs`, `TestContext`, `decode.rs`, `Option`, `mod.rs`, `run_smoke_tests`, `PaginatedResponse<T>`, `find_list_object`, `NewTagRequest`, `create_object_with_retry`, `Widget`, `unique_test_name`, `VerticalAlign`, `TestResult`, `CancelledNotificationParams`, `chat.rs`, `ViewListObjectsRequest`, `Vec`, `view_handlers.rs`, `mock.rs`, `ProcessWatcher`, `stdio_conformance.rs`, `.create_template_fixtures`, `Member`, `result.rs`, `Cli`, `pagination_limit`, `FilePreloadRequest`, `enum`, `main.rs`, `String`, `spaces.rs`, `resources.rs`, `main`, `route_aware_type_server`, `ArchiveReader`, `ui.rs`, `String`, `schema.rs`, `TagColorArg`, `Self`, `auth.rs`, `Platform`, `auth.rs`, `File`, `SyncStatus`, `HttpMetricsSnapshot`, `object_output.rs`, `execute_object_import_batches`, `load_headless_config`, `MutationNumber`, `ListTemplatesRequest`, `Result`, `Account`, `Widget`, `discovery.rs`, `EmailVerificationStatus`, `AnytypeGrpcClient`, `Text`, `PaginatedResponse<T>`, `FixtureReply`, `view.rs`, `validation.rs`, `handle`, `PeriodType`, `views.rs`, `Platform`, `StatusType`, `TemplateNamePrefillType`, `chat_messages.rs`, `.backup_space`, `TimeFormat`, `Option`, `parse_filters`, `.create_collection_type_fixture`, `.resolve_chat_target`, `auth.rs`, `FileContentResponse`, `object_generator.rs`, `AnytypeClient`, `.run`, `ListSpacesRequest`, `.create_space_fixture`, `ChatMessage`, `AnytypeError`, `handle`, `route_aware_type_server`, `crypto.rs`, `ViewMatchAccumulator`, `ListArchivedRequest`, `get_i64`, `handler_support.rs`, `FileDownloadRequest`, `ImageResizeSchema`, `fix_doc_list_indents`, `TestResultTracker`, `Message`, `object_create.rs`, `render_table`, `verify.rs`, `logging.rs`, `server.rs`, `test_collect_all_matches_total`, `properties.rs`, `.new`, `headless_integration.rs`, `protocol.rs`, `execute_object_import_batches`, `Result`, `Result`?**
  _High betweenness centrality (0.418) - this node is a cross-community bridge._
- **Why does `Result` connect `Result` to `Space Pagination`, `Integration Test Suite`, `Authentication API`, `Filtering and Sorting`, `Object Models Utilities`, `Pagination Core`, `Client Configuration`, `Type Request Models`, `Property Setter Tests`, `Test Retry Helpers`, `Changelog Concepts`, `Chat Resolution Client`, `Tag API`, `Property Value Models`, `Member Models`, `ProjectedDate`, `HTTP Retry Client`, `Property Lookup Helpers`, `Chat RPC Responses`, `Object Creation Builder`, `Chat Attachments Reactions`, `Message Content Formatting`, `Input Validation`, `Template API`, `Type Models`, `Cache Controls`, `Object Payload Models`, `Availability Verification`, `.call_tool`, `Object Update Examples`, `object_edit.rs`, `HTTP Request Methods`, `HTTP Metrics Reporting`, `Object CRUD Requests`, `Self`, `Chat Read State`, `String`, `Self`, `Chat CRUD Tests`, `Agenda Example`, `String`, `Basic Filters Example`, `Interactive Auth Example`, `stdio.rs`, `stdio.rs`, `p1_cross_space.rs`, `index.rs`, `p1_cross_space.rs`, `String`, `.new`, `with_test_context_unit`, `files.rs`, `TestContext`, `decode.rs`, `Option`, `mod.rs`, `PaginatedResponse<T>`, `find_list_object`, `NewTagRequest`, `unique_test_name`, `Widget`, `Style`, `VerticalAlign`, `TestResult`, `ResolveCandidate`, `chat.rs`, `ViewListObjectsRequest`, `Vec`, `view_handlers.rs`, `ProcessWatcher`, `stdio_conformance.rs`, `.create_template_fixtures`, `Member`, `pagination_limit`, `enum`, `main.rs`, `String`, `spaces.rs`, `resources.rs`, `main`, `route_aware_type_server`, `ViewMatchAccumulator`, `ObjectSummary`, `ArchiveReader`, `list_command`, `schema.rs`, `TagColorArg`, `AuthArgs`, `Key`, `Platform`, `auth.rs`, `object_output.rs`, `execute_object_import_batches`, `load_headless_config`, `MutationNumber`, `main`, `ListTemplatesRequest`, `Result`, `discovery.rs`, `InviteType`, `AnytypeGrpcClient`, `PeriodType`, `PaginatedResponse<T>`, `.serialize`, `view.rs`, `validation.rs`, `EmailVerificationStatus`, `handle`, `PeriodType`, `views.rs`, `Platform`, `StatusType`, `chat_messages.rs`, `.backup_space`, `Option`, `Output`, `parse_filters`, `.resolve_chat_target`, `auth.rs`, `FileContentResponse`, `object_generator.rs`, `AnytypeClient`, `.run`, `.create_space_fixture`, `AnytypeError`, `handle`, `MutationNumber`, `crypto.rs`, `main`, `test_chat_stream.rs`, `main`, `handler_support.rs`, `.fmt`, `fix_doc_list_indents`, `TestResultTracker`, `AnyMcpServer`, `object_create.rs`, `verify.rs`, `main`, `main`, `logging.rs`, `server.rs`, `test_collect_all_matches_total`, `PageLimit`, `properties.rs`, `.listen_session_events`, `.new`, `protocol.rs`, `mutation_value.rs`, `execute_object_import_batches`, `Attempt`, `Result`?**
  _High betweenness centrality (0.368) - this node is a cross-community bridge._
- **Why does `CallToolResult` connect `discovery.rs` to `Type Models`, `.new`, `deserialize_vec_or_null`, `AuthArgs`, `view_handlers.rs`, `stdio.rs`, `.call_tool`, `object_edit.rs`, `Attempt`, `headless_integration.rs`, `properties`, `handler_support.rs`, `CancelledNotificationParams`, `test_collect_all_matches_total`, `AnyMcpServer`, `object_create.rs`, `list_command`?**
  _High betweenness centrality (0.061) - this node is a cross-community bridge._
- **What connects `EDITOR_COMMAND`, `AuthCommand`, `EnabledToolset` to the rest of the system?**
  _716 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Chat Mock Server` be split into smaller, more focused modules?**
  _Cohesion score 0.09915966386554621 - nodes in this community are weakly interconnected._
- **Should `File Transfer API` be split into smaller, more focused modules?**
  _Cohesion score 0.005970149253731343 - nodes in this community are weakly interconnected._
- **Should `Integration Test Suite` be split into smaller, more focused modules?**
  _Cohesion score 0.03858250276854928 - nodes in this community are weakly interconnected._