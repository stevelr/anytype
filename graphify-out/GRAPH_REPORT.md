# Graph Report - anyr-0.5  (2026-07-21)

## Corpus Check
- 188 files · ~507,744 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 8177 nodes · 24630 edges · 250 communities (233 shown, 17 thin omitted)
- Extraction: 97% EXTRACTED · 3% INFERRED · 0% AMBIGUOUS · INFERRED: 681 edges (avg confidence: 0.8)
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
- handler_support.rs
- Changelog
- Changelog
- Setup
- anyback(1)
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
- .new
- server.rs
- test_collect_all_matches_total
- PageLimit
- properties.rs
- .listen_session_events
- Anytype Rust Tools and Clients
- prune-templates-keep-oldest.sh
- .new
- properties
- .new
- properties
- headless_integration.rs
- properties
- Result
- protocol.rs
- properties
- type
- Self
- mutation_value.rs
- execute_create
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
1. `Result` - 1448 edges
2. `Request` - 418 edges
3. `Response` - 399 edges
4. `ClientCommandsClient<T>` - 343 edges
5. `Status` - 340 edges
6. `Error` - 130 edges
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
- `log_classified_operation()` --calls--> `classify()`  [INFERRED]
  any-mcp/src/runtime.rs → anyr/src/cli/chat.rs

## Import Cycles
- None detected.

## Hyperedges (group relationships)
- **Authentication and Keystore Flow** — anytype_api_readme_anytype_api_client, anytype_api_keystores_interactive_authentication, anytype_api_keystores_authentication_token_storage, anytype_api_keystores_endpoint_specific_tokens [EXTRACTED 1.00]
- **gRPC Feature Surface** — anytype_api_changelog_grpc_backend, anytype_api_readme_grpc_api_extensions, anytype_api_readme_files_api, anytype_api_readme_chat_streaming, anytype_api_examples_readme_grpc_examples [EXTRACTED 1.00]

## Communities (250 total, 17 thin omitted)

### Community 0 - "Chat Mock Server"
Cohesion: 0.08
Nodes (22): DataModel, authorize_template_resource(), collection_fixture_ownership_error(), complete_type_object_id_snapshot(), current_space_create_response_never_enters_deletion_registry(), generic_pre_registered_id_cannot_claim_collection_provenance(), malformed_create_response_never_enters_deletion_registry(), registered_spaces() (+14 more)

### Community 1 - "File Transfer API"
Cohesion: 0.01
Nodes (323): AccountSelectTrace, AddFeatured, AddMessage, AddNotificationSubscriber, Ai, AnyNameAllocate, AnyNameIsValid, AnystoreObjectChanges (+315 more)

### Community 2 - "Space Pagination"
Cohesion: 0.06
Nodes (51): App, build_yaml_front_matter(), CachedObject, clamp_scroll(), detail_string(), detail_value_to_string(), epoch_to_rfc3339(), extract_space_id_from_archive_name() (+43 more)

### Community 3 - "Integration Test Suite"
Cohesion: 0.04
Nodes (11): Block, ClientCommandsClient<T>, AppInfo, Placeholder, Range, Request, Response, Status (+3 more)

### Community 4 - "Authentication API"
Cohesion: 0.08
Nodes (123): add_to_list(), archive_file_paths(), archive_markdown_blob(), archive_object_ids(), archive_payload_file_paths(), assert_non_tty_output_clean(), backup_selected_ids(), ChatMessageTokenCleanupGuard (+115 more)

### Community 5 - "Chat Stream Builder"
Cohesion: 0.04
Nodes (26): Amount, Cart, CartProduct, CryptoCheckout, Data, ErrorCode, Features, FileIndexingStatus (+18 more)

### Community 6 - "Filtering and Sorting"
Cohesion: 0.08
Nodes (51): cancellation_releases_permit_for_next_operation(), concurrency_limit_bounds_waiting_operations(), ControlledFailureKind, ControlledOperationError, default_control_failure_diagnostic(), eof_before_initialize_is_a_clean_shutdown(), execute_applies_end_to_end_timeout(), execute_honors_request_cancellation() (+43 more)

### Community 7 - "Object Models Utilities"
Cohesion: 0.12
Nodes (11): BackupSpaceRequest, NewSpaceRequest, Arc, Into, IntoIterator, PathBuf, Self, String (+3 more)

### Community 8 - "Pagination Core"
Cohesion: 0.11
Nodes (15): ChatAddMessageRequest, ChatEditMessageRequest, ChatHttpAddMessageRequest, ChatHttpEditMessageRequest, MessageAttachment, MessageBlock, MessageContent, AsRef (+7 more)

### Community 9 - "Client Configuration"
Cohesion: 0.07
Nodes (79): ArchiveObjectInfo, build_archive_object_index(), convert_archive_object_pb_to_markdown(), convert_archive_object_to_markdown(), convert_archive_snapshot_to_markdown(), convert_pb_json_snapshot_to_markdown(), convert_pb_snapshot_to_markdown(), convert_sample_pb_json_object_to_markdown_contains_headings() (+71 more)

### Community 10 - "Member Integration Tests"
Cohesion: 0.17
Nodes (12): description, properties, required, type, CancelledNotificationParams, reason, requestId, description (+4 more)

### Community 11 - "Type Request Models"
Cohesion: 0.09
Nodes (17): ChatService, EventStream, MockChatServer, MockChatServerHandle, Box, Default, JoinHandle, Pin (+9 more)

### Community 12 - "Property Setter Tests"
Cohesion: 0.19
Nodes (8): BoundedText, MutationDate, MutationIconText, Cow, Deserialize, JsonSchema, MAX_MUTATION_DATE_CHARS, MAX_MUTATION_ICON_TEXT_CHARS

### Community 13 - "Test Retry Helpers"
Cohesion: 0.06
Nodes (30): AnytypeClient, Color, CreateObjectRequestBody, Icon, ListObjectsRequest, NewObjectRequest, Object, object_link() (+22 more)

### Community 14 - "Client Cache"
Cohesion: 0.04
Nodes (107): Align, Align, Amend, Auth, AutoArchive, AutoRestore, Avatar, BackgroundColor (+99 more)

### Community 15 - "Process Watcher"
Cohesion: 0.15
Nodes (17): anytype_classifiers_cover_every_directly_constructible_error_variant(), anytype_error_mapping_discards_upstream_response_text(), AnytypeErrorMapping, api_error(), assert_anytype_mapping(), candidate_rich_anytype_ambiguity_maps_to_exact_tool_error(), mixed_anytype_candidates_retain_valid_alternatives(), mutation_http_rejection_allowlist_is_conservative_at_boundaries() (+9 more)

### Community 16 - "Changelog Concepts"
Cohesion: 0.31
Nodes (5): ChatEditTextRequest, ChatSendTextRequest, http_message_style(), MessageTextMark, MessageTextStyle

### Community 17 - "Chat Resolution Client"
Cohesion: 0.21
Nodes (7): authenticate(), broadcast_event(), build_chat_state(), build_event(), Future, Status, MetadataMap

### Community 18 - "Tag API"
Cohesion: 0.08
Nodes (42): AnytypeClient, BackoffPolicy, call_subscribe_last_messages(), chat_events_from_event(), chat_events_respect_sub_ids(), ChatEvent, ChatEventStream, ChatStreamBuilder (+34 more)

### Community 19 - "Property Value Models"
Cohesion: 0.06
Nodes (74): all_http_trace_levels_remain_metadata_only(), assert_public_mutation_sent_once(), assert_public_redirect_is_not_followed(), caller_supplied_reqwest_retry_policy_cannot_replay_a_mutation(), Capture, chunked_exact_limit_succeeds_without_content_length(), chunked_framing_cannot_bypass_limit_with_a_low_length_header(), content_length_exact_limit_succeeds() (+66 more)

### Community 20 - "Chat Message Models"
Cohesion: 0.15
Nodes (13): Authenticated stdio runtime, Document resources, Exact-match object edit workflow, Object archive workflow, Object create workflow, Object discovery and reads, Object update workflow, Phase 1 foundations (+5 more)

### Community 21 - "Member Models"
Cohesion: 0.16
Nodes (7): GrpcCredentials, HttpCredentials, Into, Option, Self, String, Zeroize

### Community 22 - "View Models"
Cohesion: 0.15
Nodes (14): CreateName, CreateReference, optional_body_schema(), optional_icon_schema(), optional_idempotency_schema(), optional_name_schema(), optional_properties_schema(), optional_template_schema() (+6 more)

### Community 23 - "Identifier Resolution"
Cohesion: 0.04
Nodes (87): F, with_test_context_unit(), test_collect_all(), test_create_custom_property(), test_create_multiple_objects(), test_create_with_empty_name(), test_global_search(), test_invalid_object_id() (+79 more)

### Community 24 - "Property Request Builder"
Cohesion: 0.29
Nodes (6): Added, Added, Changed, Changed, Changelog, [Unreleased]

### Community 25 - "HTTP Retry Client"
Cohesion: 0.04
Nodes (72): archive_basename(), assert_backup_args_equal(), AuthArgs, AuthCommands, backup_export_options(), backup_export_options_maps_include_flags_and_pb_json(), backup_export_options_maps_markdown_include_properties(), backup_target_always_uses_zip_extension_for_generated_name() (+64 more)

### Community 26 - "Property Lookup Helpers"
Cohesion: 0.07
Nodes (96): invalid_catalog_profile_fails_before_auth_without_echoing_its_value(), invalid_operational_setting_does_not_echo_its_value(), invalid_protocol_mode_fails_before_auth_without_echoing_its_value(), invalid_read_only_setting_fails_before_auth_without_echoing_its_value(), startup_auth_failure_is_nonzero_stderr_only_and_redacted(), unauthenticated_command(), add_to_list(), alpha_suffix() (+88 more)

### Community 27 - "Chat RPC Responses"
Cohesion: 0.05
Nodes (70): append_sse_byte(), chat_stream_diagnostic_omits_url_credentials_query_and_fragment(), chat_stream_diagnostic_path(), ChatHttpEvent, ChatHttpEventStream, ChatHttpSseState, ChatMessage, ChatMessageSearchResult (+62 more)

### Community 28 - "Object Creation Builder"
Cohesion: 0.13
Nodes (21): all_discovery_contracts_are_strict_bounded_read_tools(), property_list_tool(), DisplayName, EntityId, SpaceId, server_status_tool(), ServerStatusOutput, space_list_tool() (+13 more)

### Community 29 - "Search API"
Cohesion: 0.03
Nodes (32): AutofillMode, Code, Context, DetailsSet, DeviceAdd, DeviceState, GenericErrorResponse, Language (+24 more)

### Community 30 - "Chat Attachments Reactions"
Cohesion: 0.13
Nodes (18): checked_tag_count(), convert_property_summary(), finish_property_page(), property_format(), PropertyFormatSummary, PropertyPageItem, PropertyPageSource, PropertySummary (+10 more)

### Community 31 - "Message Content Formatting"
Cohesion: 0.07
Nodes (27): AnytypeClient, CreateTypeProperty, CreateTypeRequestBody, deserialize_vec_properties_or_null(), ListTypesRequest, NewTypeRequest, prime_cache_types(), Arc (+19 more)

### Community 32 - "Input Validation"
Cohesion: 0.10
Nodes (14): ChatDeleteMessageRequest, ChatReadAllRequest, ChatSpaceRequest, ChatToggleReactionRequest, filter_unread_messages(), grpc_attachments(), grpc_message_conversion_retains_rich_state(), grpc_read_type() (+6 more)

### Community 33 - "Template API"
Cohesion: 0.05
Nodes (38): Condition, deserialize_vec_string_or_null(), Filter, FilterExpression, FilterOperator, join_values(), Query, D (+30 more)

### Community 34 - "Type Models"
Cohesion: 0.06
Nodes (79): active_and_archived_scans_stop_at_explicit_page_and_item_bounds(), ambiguous_delete_failures_are_indeterminate_after_one_dispatch(), ambiguous_success_responses_recover_or_return_indeterminate_without_redelete(), archive_evidence(), archive_output(), archive_verification_config(), archive_verification_honors_hard_attempt_and_time_caps(), ArchivedState (+71 more)

### Community 35 - "Cache Controls"
Cohesion: 0.12
Nodes (13): QueryWithFilters, Arc<HttpClient>, deserialize_json(), HttpClient, HttpMetrics, log_and_backoff(), AtomicU64, HeaderMap (+5 more)

### Community 36 - "Object Payload Models"
Cohesion: 0.67
Nodes (3): main(), Box, run()

### Community 37 - "Availability Verification"
Cohesion: 0.06
Nodes (45): Groups, BlockParticipant, ChangePayload, Dataview, DataviewRestrictions, DetailsSet, Filter, Group (+37 more)

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
Cohesion: 0.16
Nodes (19): bool_field(), ChatMessagesPage, ChatState, HttpMessageWriteAttachment, HttpMessageWriteBody, HttpMessageWriteMark, last_modified_date(), number_field() (+11 more)

### Community 42 - "object_edit.rs"
Cohesion: 0.07
Nodes (72): apply_edits(), bounded_result(), checked_space_id(), contract_is_closed_bounded_destructive_and_defaults_match_count(), edit_input(), edited_state_matches(), EditExecution, EditInputError (+64 more)

### Community 43 - "HTTP Metrics Reporting"
Cohesion: 0.11
Nodes (27): create_test_request(), PagedResult<T>, PaginatedResponse, BoxStream, Ok, S, Self, Serialize (+19 more)

### Community 44 - "object_edit.rs"
Cohesion: 0.24
Nodes (4): Self, String, Vec, TestResultTracker

### Community 45 - "Object Layout Tests"
Cohesion: 0.08
Nodes (45): Account, Add, BlockField, Cafe, ChatPreview, Config, Details, Device (+37 more)

### Community 46 - "object_edit.rs"
Cohesion: 0.19
Nodes (31): ChatAddMessageSvc, ChatDeleteMessageSvc, ChatEditMessageSvc, ChatGetMessagesByIdsSvc, ChatGetMessagesSvc, ChatReadAllSvc, ChatReadMessagesSvc, ChatSubscribeLastMessagesSvc (+23 more)

### Community 47 - "String"
Cohesion: 0.25
Nodes (4): Contributing to stevelr/anytype, Documentation, I Have a Question, Table of Contents

### Community 48 - "Error Types"
Cohesion: 0.25
Nodes (8): any-mcp, Build, Headless integration tests, License, Protocol channel, Quick start, Source layout, Testing

### Community 49 - "Object CRUD Requests"
Cohesion: 0.03
Nodes (27): Align, Block, BlockMetaOnly, CardStyle, Condition, Config, DataviewRestriction, DateFormat (+19 more)

### Community 50 - "Self"
Cohesion: 0.16
Nodes (11): file_type_filter(), file_type_from_mime(), FileSource, FileStyle, FileType, FileUploadRequest, grpc_file_style(), grpc_file_type() (+3 more)

### Community 51 - "Chat Read State"
Cohesion: 0.20
Nodes (9): KeyStoreError, From, PathBuf, Self, VarError, KeyStore, AsRef, test_file_storage_save_and_load() (+1 more)

### Community 52 - "Object List Pagination"
Cohesion: 0.09
Nodes (50): AuthArgs, AuthCommand, AuthSource, AuthStatusArgs, build_yaml_export(), ConfigFile, detect_scope(), ExportHeaderFormat (+42 more)

### Community 53 - "Example Table Rendering"
Cohesion: 0.03
Nodes (22): ActionType, AttachmentType, Code, DataSource, DeviceNetworkType, EmailVerificationStatus, Format, IconSize (+14 more)

### Community 54 - "Object"
Cohesion: 0.33
Nodes (6): Automated harness, Client configuration evidence, Current status, External tool evidence, Released compatibility matrix, Stdio protocol verification

### Community 55 - "String"
Cohesion: 0.23
Nodes (10): AnytypeClient, ChatTarget, fixture_client_with_grpc(), public_chat_resolution_composes_bounded_http_and_grpc_discovery(), resolution_limit(), resolve_message_id_maps_order_id_and_passes_ids_through(), Option, String (+2 more)

### Community 56 - "Self"
Cohesion: 0.18
Nodes (12): canonical_returned_date(), MutationCompareError, returned_id(), returned_ids(), returned_tag_ids(), Display, Formatter, Icon (+4 more)

### Community 57 - "Chat CRUD Tests"
Cohesion: 0.05
Nodes (29): AddChatMessageResponse, chat_message_path(), ChatClient<'a>, ChatCreateRequest, ChatGetMessageRequest, ChatGetMessagesRequest, ChatHttpListMessagesRequest, ChatHttpReadMessagesRequest (+21 more)

### Community 58 - "Agenda Example"
Cohesion: 0.09
Nodes (53): backup_selected_ids(), BugDisposition, CaseStatus, choose_two_distinct_writable_spaces_cli(), choose_writable_space_cli(), clone_sqlite_with_sidecars(), cloned_test_keystore(), configure_test_keystore() (+45 more)

### Community 59 - "mod.rs"
Cohesion: 0.27
Nodes (9): chat_details(), number_value(), Option, Struct, Value, string_value(), value_bool(), value_number() (+1 more)

### Community 60 - "Value"
Cohesion: 0.07
Nodes (28): description, properties, $ref, type, description, properties, required, type (+20 more)

### Community 61 - "Value"
Cohesion: 0.25
Nodes (6): Checkbox, Date, Detail, Status, Tag, Value

### Community 62 - "File Example"
Cohesion: 0.19
Nodes (20): is_expected_member_lookup_error(), String, TestResult, test_active_member_exists(), test_get_member_by_id(), test_get_member_invalid_space(), test_get_nonexistent_member(), test_list_members() (+12 more)

### Community 63 - "String"
Cohesion: 0.12
Nodes (15): ambiguity_error_exposes_only_bounded_candidates(), bare_chat_id_discovery_has_one_global_space_and_chat_budget(), candidate_selection_filters_invalid_values_before_its_cap(), domain_candidates_deduplicate_space_type_view_chat_and_property_rows(), empty_page_server(), explicit_type_id_resolution_uses_one_direct_get_with_cache_enabled(), later_direct_view_id_wins_over_earlier_name_ambiguity(), MatchAccumulator<T> (+7 more)

### Community 64 - "Basic Filters Example"
Cohesion: 0.10
Nodes (20): AnytypeCache, Arc, AsRef, Default, Formatter, HashMap, Mutex, Option (+12 more)

### Community 65 - "Interactive Auth Example"
Cohesion: 0.07
Nodes (20): AnytypeClient, ClientConfig, extract_port(), find_grpc(), lsof_listen_ports(), lsof_listen_ports_filters_prefix(), probe_grpc_port(), ResponseLimits (+12 more)

### Community 66 - "Value"
Cohesion: 0.20
Nodes (16): all_property_object_value(), all_property_type_value(), contract_is_strict_bounded_non_null_and_uses_create_annotations(), is_canonical_plain_body(), is_plain_body_line(), normalize_create_body(), object_create_tool(), object_inner() (+8 more)

### Community 67 - "stdio.rs"
Cohesion: 0.07
Nodes (72): ServeError, add_cache(), add_complete(), bounded_reader_recovers_at_the_next_line(), cancel_all(), decode(), dispatch_modern(), drain_frame() (+64 more)

### Community 68 - "Type Property Example"
Cohesion: 0.09
Nodes (43): clone_collection_view(), collection_view_fixture_accepts_exact_new_event_identity(), collection_view_fixture_binds_object_show_root_and_exact_block(), collection_view_fixture_clone_changes_only_id_and_name(), collection_view_fixture_code_error(), collection_view_fixture_cross_checks_every_rest_visible_field(), collection_view_fixture_error(), collection_view_fixture_rejects_preexisting_or_unproven_identity() (+35 more)

### Community 69 - "stdio.rs"
Cohesion: 0.14
Nodes (26): application_profile_parser_is_exact_and_secret_safe(), config(), ConfigError, default_document_budget_is_routed_to_anytype_client(), defaults_are_bounded_and_reuse_anyr_keystore_service(), errors_name_the_variable_without_echoing_its_value(), maps_supported_anytype_environment_settings(), non_empty() (+18 more)

### Community 70 - "Consistency Retry Example"
Cohesion: 0.23
Nodes (11): ambiguous(), not_found(), Into, IntoIterator, Type, starts_with_uppercase(), type_and_property_key_resolution_bypass_cache_prime_paths(), TypeMatchMode (+3 more)

### Community 71 - "stdio.rs"
Cohesion: 0.19
Nodes (14): malformed_template_resolution(), MatchAccumulator, MatchClassification, object_candidate(), Fn, HashSet, Object, T (+6 more)

### Community 72 - "Space Search Example"
Cohesion: 0.10
Nodes (50): unique_suffix(), with_test_context(), TestResult, test_create_custom_property(), test_create_property_duplicate_key(), test_create_property_invalid_name(), test_delete_property(), test_property_key_stability() (+42 more)

### Community 73 - "p1_cross_space.rs"
Cohesion: 0.35
Nodes (5): MutationInputError, D, Into, Self, String

### Community 74 - "index.rs"
Cohesion: 0.13
Nodes (38): ArchiveIndex, build_preview(), build_preview_is_stable_and_compact(), build_preview_preserves_markdown_lines_without_truncating_headings(), collect_link_candidates(), collect_preview_strings(), collect_user_properties(), collect_user_properties_includes_array_and_object_values() (+30 more)

### Community 75 - "p1_cross_space.rs"
Cohesion: 0.22
Nodes (9): CreateInputError, fingerprint_v1_is_domain_separated_golden_and_semantically_canonical(), input_with_body(), read_request(), Display, Formatter, Into, String (+1 more)

### Community 76 - "String"
Cohesion: 0.11
Nodes (18): description, items, type, items, $ref, items, type, audience (+10 more)

### Community 77 - ".new"
Cohesion: 0.20
Nodes (17): AnytypeClient, ChatClient, ChatHttpMessageStreamRequest, dropping_stream_cancels_incomplete_transport(), mock_http_client(), one_transport_chunk_can_carry_multiple_exact_limit_events(), opening_transport_failure_discards_raw_url_and_source(), overflowing_stream_terminates_and_releases_transport_state() (+9 more)

### Community 78 - "with_test_context_unit"
Cohesion: 0.27
Nodes (9): ChatBackend, classify(), classify_messages(), OpTransport, resolve_transport(), Display, Formatter, Self (+1 more)

### Community 79 - "files.rs"
Cohesion: 0.10
Nodes (39): file_from_details(), file_from_http_upload(), file_http_metadata(), FileHttpMetadata, FileObject, FilesClient, filter_id_equal(), filter_not_empty() (+31 more)

### Community 80 - "TestContext"
Cohesion: 0.20
Nodes (9): collection_type_details(), collection_type_fixture_details_use_the_canonical_heart_layout(), Into, Struct, Type, Value, string_value(), TemplateFixtureSet (+1 more)

### Community 82 - "decode.rs"
Cohesion: 0.12
Nodes (45): build_expanded_entry_from_details(), derive_layout_name(), detail_value(), ExpandedSnapshotEntry, format_datetime_display(), format_datetime_with_tz(), format_last_modified(), format_utc_datetime_with_tz() (+37 more)

### Community 83 - "Option"
Cohesion: 0.16
Nodes (13): AnytypeClient, archived_search_request(), dataview_filter_type_in(), delete_archived_best_effort(), DeleteAllArchivedResult, DeleteBestEffortResult, ListArchivedRequest, ListArchivedRequest<'a> (+5 more)

### Community 84 - "mod.rs"
Cohesion: 0.21
Nodes (9): choose_property_name(), handle(), handle_update(), property_command(), AppContext, Option, String, update_parses_name_and_key_forms() (+1 more)

### Community 85 - "create_object_with_retry"
Cohesion: 0.12
Nodes (40): create_object_with_retry(), ensure_properties_and_type(), is_key_already_exists_error(), lookup_property_tag_with_retry(), F, Object, Tag, TestResult (+32 more)

### Community 86 - "PaginatedResponse<T>"
Cohesion: 0.20
Nodes (11): CreateFingerprintV1, fingerprint_hex(), FingerprintField, FingerprintField<'a, T>, NormalizedCreate, ObjectCreateInput, MutationProperties, Option (+3 more)

### Community 87 - "find_list_object"
Cohesion: 0.26
Nodes (5): FixtureReply, ObjectCreateHandlers, D, Duration, Self

### Community 88 - "NewTagRequest"
Cohesion: 0.09
Nodes (24): AnytypeClient, CreateTagRequest, ListTagsRequest, NewTagRequest, refresh_cached_property_tags(), Arc, Color, Filter (+16 more)

### Community 89 - "unique_test_name"
Cohesion: 0.15
Nodes (27): is_definitive_rate_limit_rejection(), retry_definitive_rate_limit(), unique_test_name(), TestResult, test_chat_discovery_requests(), test_rest_chat_messages_reactions_search_and_reads(), TestResult, test_chat_message_crud() (+19 more)

### Community 90 - "Widget"
Cohesion: 0.28
Nodes (4): file_path(), FileContentResponse, Method, StatusCode

### Community 91 - "QuickOption"
Cohesion: 0.11
Nodes (17): Client surface, Colors, alignment, backgrounds, Context and space ownership, Core model, Data sources and transport, Embeds, Error mapping, Graph validation (+9 more)

### Community 92 - "Style"
Cohesion: 0.14
Nodes (11): AnytypeDiagnostic, diagnostic_method(), grpc(), grpc_error_is_authentication(), grpc_status_is_authentication(), nested_grpc_authentication_is_structural_and_exhaustive(), Display, Duration (+3 more)

### Community 93 - "VerticalAlign"
Cohesion: 0.29
Nodes (12): AnytypeGrpcError, AuthError, BackupError, ConfigError, ConfigError, From, PathBuf, Self (+4 more)

### Community 94 - "TestResult"
Cohesion: 0.28
Nodes (7): BoundedList, BoundedList<T, MAX>, error(), D, Self, T, Vec

### Community 95 - "priority_groups.rs"
Cohesion: 0.09
Nodes (40): assert_case_registered(), case_replace_after_object_type_changed_since_backup(), case_replace_object_type_collection_with_items(), case_replace_object_type_complex_nested_object(), case_replace_object_type_custom_type_object(), case_replace_object_type_file(), case_replace_object_type_object(), case_replace_object_type_property() (+32 more)

### Community 96 - "CancelledNotificationParams"
Cohesion: 0.31
Nodes (14): explicit_type_id_resolution_rejects_safe_mismatched_identity(), fixture_client(), paged_fixture_server(), public_space_resolution_deduplicates_across_http_pages(), public_view_resolution_preserves_later_direct_id_across_pages(), Value, template_direct_id_uses_one_get_and_revalidates_identity(), template_page() (+6 more)

### Community 97 - "ResolveCandidate"
Cohesion: 0.43
Nodes (4): Display, Self, ServerBuildError, validate_catalog()

### Community 98 - "chat.rs"
Cohesion: 0.05
Nodes (49): backend_of(), blocks_json_rejected_with_transport_rest(), create_chat_object(), decode_order_id_arg(), decode_order_id_hex_roundtrip(), decode_order_id_non_hex_passthrough(), emit_message_rows(), encode_order_id_hex() (+41 more)

### Community 99 - "ViewListObjectsRequest"
Cohesion: 0.12
Nodes (23): AnytypeClient, deserialize_vec_filter_or_null(), deserialize_vec_sort_or_null(), fixture_client(), ListViewsRequest, Arc, D, Filter (+15 more)

### Community 100 - "Vec"
Cohesion: 0.05
Nodes (72): MarkRange, Field, Account, Auth, Bookmark, Chat, ChatState, Content (+64 more)

### Community 101 - "view_handlers.rs"
Cohesion: 0.07
Nodes (51): Page, ambiguous_view_name_returns_actionable_bounded_candidates(), convert_view_object_page(), convert_view_page(), fixture_client(), fixture_server(), object(), page() (+43 more)

### Community 102 - "mock.rs"
Cohesion: 0.15
Nodes (22): chat_add_value(), chat_delete_value(), chat_state_update_value(), chat_update_value(), ChatRoom, filter_match(), filter_messages(), filters_match() (+14 more)

### Community 103 - "ProcessWatcher"
Cohesion: 0.13
Nodes (25): Account, matches_process_kind(), next_test_addr(), open_session_events(), ProcessCompletionFallback, ProcessKind, ProcessWatchCancelToken, ProcessWatcher (+17 more)

### Community 104 - "stdio_conformance.rs"
Cohesion: 0.08
Nodes (52): assert_compact_wire_catalog(), assert_exact_decoder_error(), assert_exact_wire_catalog(), assert_exchange_depth(), assert_official_modern_request(), assert_official_modern_response(), assert_stdout_purity(), assert_structured_result() (+44 more)

### Community 105 - ".new"
Cohesion: 0.05
Nodes (46): description, properties, required, type, additionalProperties, description, type, description (+38 more)

### Community 106 - ".create_template_fixtures"
Cohesion: 0.10
Nodes (26): cleanup_template_resource(), collection_fixture_transport_error(), collection_fixture_transport_error_redacts_tonic_status(), collection_view_fixture_transport_error(), collection_view_fixture_transport_error_redacts_tonic_status(), complete_space_id_snapshot(), delete_space_fixture(), From (+18 more)

### Community 107 - "Member"
Cohesion: 0.10
Nodes (17): AnytypeClient, ListMembersRequest, make_member(), Member, MemberRequest, MemberResponse, MemberRole, MemberStatus (+9 more)

### Community 108 - "result.rs"
Cohesion: 0.29
Nodes (8): candidate_is_safe(), compare_candidates(), compare_duplicate_representatives(), insert_bounded_candidate(), property_candidate(), ResolveCandidate, Property, Ordering

### Community 109 - "Cli"
Cohesion: 0.24
Nodes (7): FileDownloadDestination, FileHttpUploadRequest, FileUploadResponse, http_upload_file(), Bytes, PathBuf, simple_path_and_byte_uploads_select_rest()

### Community 110 - "pagination_limit"
Cohesion: 0.07
Nodes (41): handle(), list_command(), list_objects_accepts_view(), list_objects_requires_view(), AppContext, ListArgs, handle(), AppContext (+33 more)

### Community 111 - "FilePreloadRequest"
Cohesion: 0.11
Nodes (17): chat_details_keys(), chat_layout_filter(), chat_search(), chat_search_space(), ChatGetRequest, ChatHttpListRequest, ChatListResult, filter_id_equal() (+9 more)

### Community 112 - "enum"
Cohesion: 0.24
Nodes (6): hex_nibble(), unhex(), Display, Formatter, ValidationCode, ValidationError

### Community 113 - "main.rs"
Cohesion: 0.16
Nodes (29): init_logging(), auth_login(), auth_logout(), AuthCommand, check_auth_status(), Cli, Commands, copy_link_command() (+21 more)

### Community 114 - "String"
Cohesion: 0.08
Nodes (62): ChatReadTypeArg, AppContext, AuthArgs, AuthCommands, build_client(), ChatArgs, ChatCommands, ChatMessagesArgs (+54 more)

### Community 115 - "FileTypeArg"
Cohesion: 0.36
Nodes (6): FileStyle, FileType, From, Self, FileStyleArg, FileTypeArg

### Community 116 - "spaces.rs"
Cohesion: 0.10
Nodes (16): archived_object_from_search_result(), CreateSpaceRequestBody, dataview_filter_checkbox_equal(), ListSpacesRequest, normalized_search_result_id(), prime_cache_spaces(), Filter, Icon (+8 more)

### Community 117 - "resources.rs"
Cohesion: 0.09
Nodes (50): AnytypeResources, body_above_100k_chars_fails_without_silent_truncation(), cancellation_aborts_a_delayed_resource_read(), canonical_search_get_and_write_uri_type_round_trips_strictly(), controlled_error(), convert_object(), document_response_byte_ceiling_is_exact_before_conversion(), error_code() (+42 more)

### Community 118 - "main"
Cohesion: 0.27
Nodes (3): AnytypeClient, F, Into

### Community 119 - "route_aware_type_server"
Cohesion: 0.35
Nodes (11): cleanup_by_ids(), cleanup_by_name_prefix(), create_object_once(), create_object_with_retry(), generate_fixture(), GeneratedFixture, GeneratedObject, AnytypeClient (+3 more)

### Community 120 - "ViewMatchAccumulator"
Cohesion: 0.17
Nodes (13): Change, ChangeNoSnapshot, Content, DocumentCreate, DocumentDelete, FileKeys, HashMap, Snapshot (+5 more)

### Community 121 - "anyr"
Cohesion: 0.25
Nodes (8): Before Submitting a Bug Report, Before Submitting an Enhancement, How Do I Submit a Good Bug Report?, How Do I Submit a Good Enhancement Suggestion?, I Want To Contribute, Reporting Bugs, Submitting PRs, Suggesting Enhancements

### Community 122 - "ObjectSummary"
Cohesion: 0.05
Nodes (28): BoundedText<MAX>, DomainValueError, LastModified, LastModifiedError, object_summary_serializes_with_canonical_resource_uri(), ObjectResourceUri, ObjectSummary, AsRef (+20 more)

### Community 123 - "ArchiveReader"
Cohesion: 0.12
Nodes (21): ArchiveFileEntry, ArchiveReader, ArchiveSourceKind, infer_object_id_from_snapshot_path(), infer_object_ids_from_files(), looks_like_content_id(), reader_lists_and_reads_directory_archive(), reader_lists_and_reads_zip_archive() (+13 more)

### Community 124 - "ui.rs"
Cohesion: 0.15
Nodes (33): buffer_to_lines(), draw(), draw_contents_panel(), draw_footer(), draw_help_overlay(), draw_links_panel(), draw_main_area(), draw_metadata_panel() (+25 more)

### Community 125 - "String"
Cohesion: 0.07
Nodes (42): columns_for_items(), default_columns(), handle(), load_property_names(), object_value_for_relation(), override_columns(), AppContext, BTreeMap (+34 more)

### Community 126 - "list_command"
Cohesion: 0.20
Nodes (11): generic_set_property_application_omits_format_and_preserves_canonical_values(), ids(), MutationIds, MutationProperty, MutationPropertyKey, normalized_properties(), normalized_values_serialize_deterministically_for_future_fingerprints(), properties_are_bounded_sorted_and_duplicate_keys_reject() (+3 more)

### Community 127 - "schema.rs"
Cohesion: 0.07
Nodes (54): BooleanTrueInput, BooleanTrueSchema, contains_keyword(), EmptyNestedInput, EmptySchema, FreeFormMapInput, FreeFormValueInput, has_only_keywords() (+46 more)

### Community 128 - "TagColorArg"
Cohesion: 0.16
Nodes (13): &'a mut PaginatedResponse<T>, &'a PagedResult<T>, &'a PaginatedResponse<T>, next_response_iter(), PagedResult, Refill, Arc, IntoIterator (+5 more)

### Community 129 - "Processor"
Cohesion: 0.29
Nodes (7): Color, Icon, MutationColor, MutationContractInput, MutationIcon, From, MutationProperties

### Community 130 - "AuthArgs"
Cohesion: 0.22
Nodes (7): error_contains_stable_structured_and_compact_json_content(), ResultEncodingError, Display, Formatter, SummaryInput, SummaryResult, tool_error()

### Community 131 - "Self"
Cohesion: 0.16
Nodes (5): FileListRequest, FileSearchRequest, IntoIterator, Self, size_filter()

### Community 132 - "deserialize_vec_or_null"
Cohesion: 0.35
Nodes (8): default_platform_keyring(), init_keystore(), parse_keystore(), Arc, HashMap, Path, store_from_env(), CredentialStore

### Community 133 - "Description"
Cohesion: 0.31
Nodes (7): definitive_rate_limit_retry_delay(), Duration, api_error(), definitive_rate_limit_backoff_is_finite_and_bounded(), definitive_rate_limit_retry_exhausts_at_the_finite_attempt_cap(), indeterminate_failure_is_never_retried(), only_typed_http_429_is_a_definitive_retryable_rejection()

### Community 134 - "auth.rs"
Cohesion: 0.26
Nodes (11): AuthStatus, CreateApiKeyRequest, CreateApiKeyResponse, CreateChallengeRequest, CreateChallengeResponse, GrpcStatus, HttpStatus, KeyStoreStatus (+3 more)

### Community 135 - "Key"
Cohesion: 0.43
Nodes (4): non_null_body_offset_schema(), optional_non_null_schema(), Schema, SchemaGenerator

### Community 136 - "Platform"
Cohesion: 0.15
Nodes (9): diagnostic_path(), diagnostic_path_keeps_only_bounded_non_control_path_context(), has_invalid_percent_encoding(), HttpMetricsSnapshot, parse_diagnostic_target(), Display, Formatter, Option (+1 more)

### Community 137 - "auth.rs"
Cohesion: 0.18
Nodes (20): create_local_link_challenge(), create_session(), create_session_token(), create_session_token_from_account_key(), create_session_token_from_app_key(), LocalLinkCredentials, AsRef, Channel (+12 more)

### Community 138 - "File"
Cohesion: 0.11
Nodes (16): Bookmark, Description, FaviconHash, File, Hash, ImageHash, Mime, Name (+8 more)

### Community 139 - "SyncStatus"
Cohesion: 0.29
Nodes (4): fmt_masked(), KeyStoreType, Display, Formatter

### Community 141 - "object_output.rs"
Cohesion: 0.07
Nodes (53): all_properties(), all_property_variants_have_closed_bounded_wire_forms(), bounded(), bounded_date(), bounded_values(), cursor_projection_normalization_is_order_insensitive(), last_modified(), malformed_oversized_and_duplicate_selected_values_fail_closed() (+45 more)

### Community 142 - ".with_interceptor"
Cohesion: 0.33
Nodes (4): F, T, InterceptedService, Uri

### Community 143 - "execute_object_import_batches"
Cohesion: 0.08
Nodes (48): ObjectDescriptor, apply_import_response(), ArchiveCmpChanged, ArchiveCmpObject, ArchiveCmpReport, BackupSelection, build_archive_cmp_report(), cmp_value_to_text() (+40 more)

### Community 144 - ".serialize"
Cohesion: 0.33
Nodes (6): Any, panic_payload(), Box, Notification, NotificationCreate, Send

### Community 145 - "MutationNumber"
Cohesion: 0.70
Nodes (3): canonical_returned_number(), MutationNumber, Number

### Community 146 - "main"
Cohesion: 0.50
Nodes (4): find_list_object(), main(), Object, Option

### Community 147 - "ListTemplatesRequest"
Cohesion: 0.19
Nodes (12): AnytypeClient, ListTemplatesRequest, Arc, Filter, Into, Object, Option, Self (+4 more)

### Community 148 - "Result"
Cohesion: 0.23
Nodes (28): exit_code(), main(), Box, T, with_token(), auth_status(), connect(), disable_sharing() (+20 more)

### Community 149 - "$defs"
Cohesion: 0.04
Nodes (68): description, properties, type, description, properties, type, description, properties (+60 more)

### Community 150 - "ViewMatchAccumulator"
Cohesion: 0.67
Nodes (3): TestResult, test_collect_all_matches_total(), test_stream_matches_collect_all()

### Community 152 - "Widget"
Cohesion: 0.50
Nodes (4): Limit, ViewId, Widget, Layout

### Community 154 - "discovery.rs"
Cohesion: 0.08
Nodes (59): assert_error(), concise_summaries_preserve_closed_fields_only(), convert_space_summary(), convert_tag_summary(), convert_type_summary(), cursor_from(), DiscoveryHandlers, domain_handler_error() (+51 more)

### Community 155 - "EmailVerificationStatus"
Cohesion: 0.33
Nodes (6): ChatMessage, IdentityList, Reactions, HashMap, MessageBlock, MessageContent

### Community 156 - "InviteType"
Cohesion: 0.50
Nodes (4): main(), MessageContent, Object, status_color()

### Community 157 - "AnytypeGrpcClient"
Cohesion: 0.19
Nodes (10): AnytypeGrpcClient, AnytypeGrpcConfig, default_grpc_endpoint(), AsRef, Channel, Default, Into, Self (+2 more)

### Community 158 - "Text"
Cohesion: 0.11
Nodes (18): Checked, Color, Div, IconEmoji, IconImage, Latex, Link, Marks (+10 more)

### Community 159 - "PeriodType"
Cohesion: 0.38
Nodes (5): Formatter, ColorArg, init_tracing(), main(), run()

### Community 160 - "PaginatedResponse<T>"
Cohesion: 0.29
Nodes (3): PaginatedResponse<T>, Iter, IterMut

### Community 161 - "FixtureReply"
Cohesion: 0.70
Nodes (3): View, view_candidate(), ViewMatchAccumulator

### Community 163 - ".serialize"
Cohesion: 0.50
Nodes (3): AmbiguityCandidatesError, Display, Formatter

### Community 164 - "view.rs"
Cohesion: 0.35
Nodes (10): build_member_identity_map(), load_member_cache(), MemberCache, parse_member_identity(), resolve_member_name(), AppContext, HashMap, Option (+2 more)

### Community 165 - "TestAnyrCommands"
Cohesion: 0.17
Nodes (7): anyr_bin(), base_env(), run_anyr(), run_anyr_json(), run_help(), TestAnyrCommands, CompletedProcess

### Community 166 - "validation.rs"
Cohesion: 0.12
Nodes (17): body_limits_and_exact_unicode_boundaries(), BodyCharLimit, BodyChunk, BodyChunkInput, BodyOffset, chunk_body(), errors_are_fixed_and_secret_safe(), every_list_and_filter_cap_is_enforced() (+9 more)

### Community 170 - "Changelog"
Cohesion: 0.09
Nodes (21): [0.2.2] - anyr - 2026-01-12, [0.2.3] - anyr - 2026-01-12, [0.2.4] - anyr - 2026-01-17, [0.3.0] - anyr - 2026-01-28, [0.4.0] - anyr - 2026-02-16, [0.4.1], Added, Added (+13 more)

### Community 171 - "handle"
Cohesion: 0.06
Nodes (35): apply_file_filters_list(), apply_file_filters_search(), delete_accepts_permanent(), delete_defaults_to_non_permanent(), discard_preload_parses_space_and_file_id(), download_http(), download_parses_space_object_and_rest_options(), file_command() (+27 more)

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
Cohesion: 0.21
Nodes (15): handle(), mode_clear_when_clear_flag(), mode_merge_when_add_properties(), mode_replace_parses_set_properties(), mode_unchanged_when_no_flags(), AppContext, String, Vec (+7 more)

### Community 189 - "auth.rs"
Cohesion: 0.17
Nodes (20): credentials_summary(), credentials_summary_distinguishes_missing_grpc(), credentials_summary_distinguishes_missing_http(), credentials_summary_marks_both_missing(), credentials_summary_marks_both_present(), find_grpc_cmd(), handle(), HeadlessConfig (+12 more)

### Community 190 - "FileContentResponse"
Cohesion: 0.11
Nodes (16): AnytypeClient, conditional_not_modified_status_is_preserved(), FileContentRequest, FileDeleteRequest, FileDiscardPreloadRequest, FileGetRequest, FilesClient<'a>, head_returns_file_metadata_without_a_body() (+8 more)

### Community 198 - "AnytypeError"
Cohesion: 0.05
Nodes (42): main(), main(), main(), main(), main(), main(), main(), main() (+34 more)

### Community 199 - "error.rs"
Cohesion: 0.14
Nodes (11): ambiguity_candidates_are_bounded(), candidate(), ErrorCandidate, EntityId, IntoIterator, Self, TryFrom, Vec (+3 more)

### Community 200 - "handle"
Cohesion: 0.28
Nodes (14): handle(), HeadlessConfig, login(), logout(), AppContext, AuthArgs, Option, PathBuf (+6 more)

### Community 203 - "crypto.rs"
Cohesion: 0.32
Nodes (11): crc16_xmodem(), derive_keys_from_mnemonic(), derive_keys_from_mnemonic_go_vector(), encode_account_id(), String, slip10_derive_child(), slip10_derive_master(), slip10_derive_path() (+3 more)

### Community 206 - "test_chat_stream.rs"
Cohesion: 0.27
Nodes (10): chat_stream_receives_messages(), chat_stream_reconnects_after_disconnect(), rest_chat_stream_receives_initial_message(), AnytypeClient, F, SocketAddr, setup_mock_client(), wait_for_event() (+2 more)

### Community 214 - "handler_support.rs"
Cohesion: 0.09
Nodes (47): assert_error(), begin_page(), BoundedResult, continuation_advances_by_upstream_window_not_sparse_item_count(), conversion_and_encoding_failures_emit_one_safe_failure_diagnostic(), cursor_binding_includes_tool_limit_and_normalized_non_cursor_params(), CursorBinding, entropy_and_runtime_control_failures_map_to_fixed_errors() (+39 more)

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
Cohesion: 0.18
Nodes (6): FileDownloadRequest, FilePreloadRequest, preload_source_tracks_url_and_path(), rich_and_url_uploads_select_grpc(), AsRef, Path

### Community 228 - "ImageResizeSchema"
Cohesion: 0.35
Nodes (11): FileInfo, FileKeys, ImageResizeSchema, Link, HashMap, Link, Option, String (+3 more)

### Community 235 - "fix_doc_list_indents"
Cohesion: 0.42
Nodes (8): fix_doc_list_indents(), indent_doc_list_continuation(), indent_doc_list_line(), main(), Box, Option, PathBuf, String

### Community 239 - "TestResultTracker"
Cohesion: 0.08
Nodes (29): cursor_binding_tampering_and_process_expiry(), cursor_parts(), cursor_registry_evicts_oldest_state_at_its_cap(), CursorStore, CursorStoreError, CursorToken, hex(), malformed_unknown_version_and_canonical_query() (+21 more)

### Community 241 - "Message"
Cohesion: 0.22
Nodes (9): BlockUpdate, DropFiles, Export, Import, Message, Migration, PreloadFile, Value (+1 more)

### Community 249 - "anyback"
Cohesion: 0.22
Nodes (8): anyback, Commands, Development, Features, Integrity Testing, Library Crate, Restore Transport, Usage Notes

### Community 251 - "AnyMcpServer"
Cohesion: 0.10
Nodes (27): AnyMcpServer, decode_arguments(), invalid_arguments(), reject_static_cursor(), Arc, CancellationToken, ErrorData, JsonObject (+19 more)

### Community 253 - "object_create.rs"
Cohesion: 0.28
Nodes (25): all_shared_property_and_icon_forms_reach_one_canonical_create_payload(), first_postdispatch_failures_are_fixed_and_key_retry_never_posts_twice(), fixture(), identical_sequential_and_concurrent_keyed_calls_create_once(), input(), mismatched_key_reuse_and_read_only_cached_call_do_no_io(), named_space_type_and_template_are_bounded_and_revalidated_before_create(), no_request_fixture() (+17 more)

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
Nodes (5): format_row(), format_separator(), render_table(), String, Vec

### Community 274 - "verify.rs"
Cohesion: 0.13
Nodes (28): api_error(), availability_wrapper_preserves_first_success_behavior(), config(), dropped_verifier_drops_in_flight_fetch_without_retaining_value(), exact_attempt_cap_is_enforced_for_zero_delay(), legacy_availability_retry_classifier_preserves_exact_parity(), legacy_retryable(), next_delay() (+20 more)

### Community 289 - "logging.rs"
Cohesion: 0.09
Nodes (30): build_filter(), Capture, capture_default(), dependency_payload_targets_cannot_override_metadata_deny_filter(), ensure_trace_interest(), has_target_prefix(), init(), LoggingError (+22 more)

### Community 295 - "raycast-edit-anytype.sh"
Cohesion: 0.67
Nodes (3): EDITOR_COMMAND, notify(), raycast-edit-anytype.sh script

### Community 297 - ".new"
Cohesion: 0.12
Nodes (32): convert_search_page(), convert_search_response(), decode_and_dispatch_search(), handler_classifies_upstream_failure_without_exposing_endpoint(), invalid_explicit_and_corrupt_resolved_type_keys_fail_closed(), mock_http(), nonempty_values(), null_and_unsafe_filter_ids_fail_at_wire_decode_without_io() (+24 more)

### Community 301 - "server.rs"
Cohesion: 0.12
Nodes (38): ApplicationProfile, assert_catalog_contracts(), assert_valid_representative(), audit_schema(), capabilities_are_static_complete_and_never_advertise_list_changed(), catalog_entries_equal_the_original_typed_contracts(), catalog_snapshot(), compact_omissions_are_unknown_while_read_only_edit_fails_closed() (+30 more)

### Community 314 - "test_collect_all_matches_total"
Cohesion: 0.06
Nodes (51): array_condition(), ArrayCondition, checkbox_and_number_filters_are_forwarded_without_rewriting(), checkbox_condition(), CheckboxCondition, chunked_get_hashes_complete_unicode_body_without_leaking_remainder(), contracts_are_strict_bounded_and_body_is_optional_non_null(), date_condition() (+43 more)

### Community 315 - "PageLimit"
Cohesion: 0.07
Nodes (28): DiscoveryReference, PropertyListInput, PropertyPageParams, Deserialize, JsonSchema, Option, SpaceListInput, TagListInput (+20 more)

### Community 322 - "properties.rs"
Cohesion: 0.04
Nodes (61): property(), AnytypeClient, assert_malformed_tag_pagination(), CreatePropertyRequestBody, deserialize_vec_string_or_null(), deserialize_vec_tag_or_null(), direct_property_get_is_cache_independent_and_exactly_scoped(), direct_property_get_validates_both_ids_before_io() (+53 more)

### Community 366 - ".new"
Cohesion: 0.18
Nodes (21): ambiguous_scans_also_fail_when_candidate_completeness_exceeds_the_limit(), candidate_membership_is_independent_of_input_order(), chat_id_with_space_passes_through(), classify_matches(), distinct_stable_ids_produce_deterministic_candidates(), duplicate_rows_for_one_stable_id_resolve_uniquely(), invalid_candidates_before_a_valid_match_do_not_hide_it(), match_classification_preserves_zero_and_one_items() (+13 more)

### Community 377 - "properties"
Cohesion: 0.07
Nodes (29): description, properties, type, description, enum, type, description, $ref (+21 more)

### Community 387 - ".new"
Cohesion: 0.06
Nodes (100): HandlerError, Display, From, BodySha256, checked_effective_type(), checked_space_id(), checked_type_key(), closed_property_type_and_icon_values_are_sent_and_verified() (+92 more)

### Community 388 - "properties"
Cohesion: 0.06
Nodes (31): description, properties, required, type, CreateMessageRequestParams, description, enum, type (+23 more)

### Community 401 - "headless_integration.rs"
Cohesion: 0.23
Nodes (26): active_contains(), archived_contains(), arguments(), assert_archive_evidence(), assert_collection_view_continuation(), assert_cursor_continuation(), assert_fixture_space_continuation(), assert_fixture_template_continuation() (+18 more)

### Community 403 - "properties"
Cohesion: 0.09
Nodes (23): required, required, description, required, type, description, required, type (+15 more)

### Community 422 - "Result"
Cohesion: 0.14
Nodes (13): AnytypeReference, FilterNumber, FilterNumberError, AsRef, Cow, D, Deserialize, Display (+5 more)

### Community 441 - "protocol.rs"
Cohesion: 0.09
Nodes (27): contract(), ControlledFailurePolicy, execute_handler(), execute_mutation_handler(), execute_prepared_handler(), execute_prepared_with_policy(), execution_tool_error(), HandlerExecutionError (+19 more)

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
Cohesion: 0.15
Nodes (12): comparison_classifies_bounded_and_malformed_upstream_values(), dates_are_bounded_rfc3339_and_canonical_utc(), ids_cap_raw_input_before_sorting_and_deduplication(), only_documented_empty_values_compare_as_missing_clears(), property(), PropertyCollector, returned_domain_error(), Value (+4 more)

### Community 474 - "execute_create"
Cohesion: 0.26
Nodes (13): HandlerOperationError, execute_create(), indeterminate_operation(), EntityId, Object, ObjectId, SpaceId, Type (+5 more)

### Community 477 - "execute_object_import_batches"
Cohesion: 0.09
Nodes (39): aggregate_import_responses(), AppContext, build_import_plan(), build_import_plan_infers_ids_without_manifest_from_directory(), build_import_plan_infers_ids_without_manifest_from_zip(), build_import_plan_uses_archive_path_directly(), dir_contains_pb_or_json(), execute_object_import() (+31 more)

### Community 478 - "Result"
Cohesion: 0.03
Nodes (19): Formatter, Formatter, Path, run_inspector(), init_tracing(), main(), run(), main() (+11 more)

### Community 491 - "Attempt"
Cohesion: 0.15
Nodes (20): MutationProgress, MutationStage, Arc, Attempt, BeginAttempt, CreateDisposition, CreateExecution, finish_supervised_execution() (+12 more)

### Community 493 - "Result"
Cohesion: 0.23
Nodes (15): canonical_json(), compact_canonical_json(), json_pointer_token(), numeric_boundary(), ReviewedTokenBudget, HashMap, HashSet, Map (+7 more)

### Community 518 - "CompleteResult"
Cohesion: 0.17
Nodes (12): properties, description, type, hasMore, total, values, description, type (+4 more)

### Community 552 - ".call_tool"
Cohesion: 0.12
Nodes (15): CancellationToolServer, Arc, Future, Notify, Option, Output, RequestContext, RoleServer (+7 more)

### Community 553 - "ElicitRequestFormParams"
Cohesion: 0.12
Nodes (17): ElicitRequestFormParams, ElicitRequestURLParams, description, properties, required, type, description, properties (+9 more)

### Community 585 - "jsonrpc"
Cohesion: 0.30
Nodes (15): required, required, required, required, required, required, required, required (+7 more)

### Community 595 - "properties"
Cohesion: 0.14
Nodes (14): description, format, type, properties, required, type, BlobResourceContents, blob (+6 more)

### Community 597 - "run_smoke_tests"
Cohesion: 0.17
Nodes (16): Instant, Iter, TestContext, TestResults, run_smoke_tests(), smoke_test(), test_filters(), test_members_api() (+8 more)

### Community 631 - "CancelledNotificationParams"
Cohesion: 0.07
Nodes (29): properties, properties, required, type, anyOf, description, type, properties (+21 more)

### Community 655 - "load_headless_config"
Cohesion: 0.39
Nodes (8): AnytypeHeadlessConfig, default_headless_config_path(), load_headless_config(), ConfigError, Option, Path, PathBuf, String

### Community 774 - "Live-test mutation rate-limit audit"
Cohesion: 0.50
Nodes (3): Deliberate exclusions, Live-test mutation rate-limit audit, Retried setup inventory

## Knowledge Gaps
- **701 isolated node(s):** `EDITOR_COMMAND`, `AuthCommand`, `EnabledToolset`, `EmptyPageParams`, `SpacePageParams` (+696 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **17 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `Result` connect `Result` to `Space Pagination`, `Integration Test Suite`, `Authentication API`, `Filtering and Sorting`, `Client Configuration`, `Type Request Models`, `Test Retry Helpers`, `Chat Resolution Client`, `Tag API`, `Property Value Models`, `Member Models`, `HTTP Retry Client`, `Property Lookup Helpers`, `Chat RPC Responses`, `Object Creation Builder`, `Chat Attachments Reactions`, `Message Content Formatting`, `Input Validation`, `Template API`, `Type Models`, `Cache Controls`, `Object Payload Models`, `Availability Verification`, `.call_tool`, `Object Update Examples`, `object_edit.rs`, `HTTP Request Methods`, `HTTP Metrics Reporting`, `Object CRUD Requests`, `Self`, `Chat Read State`, `Object List Pagination`, `String`, `Self`, `Chat CRUD Tests`, `Agenda Example`, `Basic Filters Example`, `Interactive Auth Example`, `Value`, `stdio.rs`, `Type Property Example`, `stdio.rs`, `Consistency Retry Example`, `stdio.rs`, `p1_cross_space.rs`, `index.rs`, `p1_cross_space.rs`, `with_test_context_unit`, `files.rs`, `decode.rs`, `Option`, `mod.rs`, `PaginatedResponse<T>`, `find_list_object`, `NewTagRequest`, `unique_test_name`, `Widget`, `Style`, `TestResult`, `CancelledNotificationParams`, `ResolveCandidate`, `chat.rs`, `ViewListObjectsRequest`, `Vec`, `view_handlers.rs`, `ProcessWatcher`, `stdio_conformance.rs`, `.create_template_fixtures`, `Member`, `Cli`, `pagination_limit`, `FilePreloadRequest`, `enum`, `main.rs`, `String`, `spaces.rs`, `resources.rs`, `main`, `route_aware_type_server`, `ObjectSummary`, `ArchiveReader`, `String`, `list_command`, `schema.rs`, `TagColorArg`, `AuthArgs`, `deserialize_vec_or_null`, `Platform`, `auth.rs`, `SyncStatus`, `object_output.rs`, `execute_object_import_batches`, `load_headless_config`, `MutationNumber`, `main`, `ListTemplatesRequest`, `Result`, `discovery.rs`, `InviteType`, `AnytypeGrpcClient`, `PeriodType`, `ChatSearchMessagesRequest`, `.serialize`, `view.rs`, `validation.rs`, `handle`, `views.rs`, `chat_messages.rs`, `.backup_space`, `parse_filters`, `auth.rs`, `FileContentResponse`, `AnytypeError`, `error.rs`, `handle`, `crypto.rs`, `test_chat_stream.rs`, `handler_support.rs`, `FileDownloadRequest`, `fix_doc_list_indents`, `TestResultTracker`, `AnyMcpServer`, `object_create.rs`, `verify.rs`, `main`, `main`, `logging.rs`, `.new`, `server.rs`, `test_collect_all_matches_total`, `PageLimit`, `properties.rs`, `.listen_session_events`, `.new`, `.new`, `Result`, `protocol.rs`, `Self`, `execute_create`, `execute_object_import_batches`, `Result`?**
  _High betweenness centrality (0.658) - this node is a cross-community bridge._
- **Why does `CallToolResult` connect `discovery.rs` to `Type Models`, `.new`, `AuthArgs`, `view_handlers.rs`, `.call_tool`, `.new`, `object_edit.rs`, `Attempt`, `headless_integration.rs`, `properties`, `$defs`, `handler_support.rs`, `CancelledNotificationParams`, `protocol.rs`, `AnyMcpServer`, `object_create.rs`?**
  _High betweenness centrality (0.079) - this node is a cross-community bridge._
- **Why does `$defs` connect `$defs` to `properties`, `.new`, `Member Integration Tests`, `ElicitRequestFormParams`, `Value`, `properties`, `properties`, `properties`, `properties`, `discovery.rs`, `AnyMcpServer`, `type`?**
  _High betweenness centrality (0.059) - this node is a cross-community bridge._
- **What connects `EDITOR_COMMAND`, `AuthCommand`, `EnabledToolset` to the rest of the system?**
  _701 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Chat Mock Server` be split into smaller, more focused modules?**
  _Cohesion score 0.08170731707317073 - nodes in this community are weakly interconnected._
- **Should `File Transfer API` be split into smaller, more focused modules?**
  _Cohesion score 0.006172839506172839 - nodes in this community are weakly interconnected._
- **Should `Space Pagination` be split into smaller, more focused modules?**
  _Cohesion score 0.06405856783344772 - nodes in this community are weakly interconnected._