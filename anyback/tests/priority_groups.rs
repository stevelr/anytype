//! Priority group coverage map for restore/import scenarios.
//!
//! This suite is intentionally lightweight: each case is a named test handle
//! tied to either an existing executable e2e test or a planned gap.
//! Use `group1` and `group2` to print consolidated status for P1/P2.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaseState {
    Works,
    Fails,
    Untested,
}

#[derive(Debug, Clone, Copy)]
struct CaseDef {
    name: &'static str,
    state: CaseState,
    mapped_test: Option<&'static str>,
    note: &'static str,
}

macro_rules! case_fn {
    ($fn_name:ident, $case_name:literal, $state:expr, $mapped:expr, $note:literal) => {
        fn $fn_name() -> CaseDef {
            CaseDef {
                name: $case_name,
                state: $state,
                mapped_test: $mapped,
                note: $note,
            }
        }
    };
}

// P1: restore/import into different space, field preservation, plus full backup to new space.
case_fn!(
    restore_non_archived_object_between_spaces_preserves_fields,
    "restore_non_archived_object_between_spaces_preserves_fields",
    CaseState::Works,
    Some("p1_restore_non_archived_object_between_spaces_preserves_fields"),
    "Executable P1 cross-space field-preservation test passes."
);
case_fn!(
    restore_non_archived_image_between_spaces_preserves_fields,
    "restore_non_archived_image_between_spaces_preserves_fields",
    CaseState::Fails,
    Some("p1_restore_non_archived_image_between_spaces_preserves_fields"),
    "Executable P1 test fails: restore reports success but destination has no discoverable restored image by token."
);
case_fn!(
    restore_non_archived_pdf_between_spaces_preserves_fields,
    "restore_non_archived_pdf_between_spaces_preserves_fields",
    CaseState::Fails,
    Some("p1_restore_non_archived_pdf_between_spaces_preserves_fields"),
    "Executable P1 test fails: restore reports success but destination has no discoverable restored pdf by token."
);
case_fn!(
    restore_type_object_between_spaces_preserves_fields,
    "restore_type_object_between_spaces_preserves_fields",
    CaseState::Fails,
    Some("p1_restore_type_object_between_spaces_preserves_fields"),
    "Executable P1 test fails: restored type name exists but key is mutated (p1_* -> p_1_*)."
);
case_fn!(
    restore_property_object_between_spaces_preserves_fields,
    "restore_property_object_between_spaces_preserves_fields",
    CaseState::Fails,
    Some("p1_restore_property_object_between_spaces_preserves_fields"),
    "Executable P1 test fails: restored property name exists but key is mutated (p1_* -> p_1_*)."
);
case_fn!(
    restore_collection_and_items_between_spaces_preserves_fields,
    "restore_collection_and_items_between_spaces_preserves_fields",
    CaseState::Works,
    Some("p1_restore_collection_and_items_between_spaces_preserves_fields"),
    "Executable P1 test passes for collection and contained item relationship."
);
case_fn!(
    restore_custom_type_object_between_spaces_preserves_fields,
    "restore_custom_type_object_between_spaces_preserves_fields",
    CaseState::Works,
    Some("p1_restore_custom_type_object_between_spaces_preserves_fields"),
    "Executable P1 test passes after waiting for type relation materialization."
);
case_fn!(
    restore_full_backup_into_new_space,
    "restore_full_backup_into_new_space",
    CaseState::Works,
    Some("e2e_backup_create_full_then_restore_into_new_space_path"),
    "Basic cross-space full backup restore path covered via path import."
);

// P2: same-space permanent delete restore, replace semantics, type changes, chain restore, unarchive on replace.
case_fn!(
    restore_permanently_deleted_object_same_space,
    "restore_permanently_deleted_object_same_space",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_object"),
    "Reproduced: permanently deleted object not discoverable after restore."
);
case_fn!(
    restore_permanently_deleted_file_same_space,
    "restore_permanently_deleted_file_same_space",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_file_same_space"),
    "Executable test fails: permanently deleted file object not discoverable after restore."
);
case_fn!(
    restore_permanently_deleted_type_same_space,
    "restore_permanently_deleted_type_same_space",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_type_same_space"),
    "Executable test fails: permanently deleted type object not restored."
);
case_fn!(
    restore_permanently_deleted_property_same_space,
    "restore_permanently_deleted_property_same_space",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_property_same_space"),
    "Executable test fails: permanently deleted property object not restored."
);
case_fn!(
    restore_permanently_deleted_collection_with_items_same_space,
    "restore_permanently_deleted_collection_with_items_same_space",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_collection_with_items_same_space"),
    "Executable test fails: permanently deleted collection+items graph not restored."
);
case_fn!(
    restore_permanently_deleted_preserves_date_fields,
    "restore_permanently_deleted_preserves_date_fields",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_object"),
    "Cannot verify date preservation while object recovery itself fails."
);
case_fn!(
    restore_permanently_deleted_object_variant_object,
    "restore_permanently_deleted_object_variant_object",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_object"),
    "Object variant known failing."
);
case_fn!(
    restore_permanently_deleted_object_variant_file,
    "restore_permanently_deleted_object_variant_file",
    CaseState::Fails,
    Some("e2e_restore_recovers_permanently_deleted_file_same_space"),
    "File variant now covered and currently failing."
);
case_fn!(
    replace_restores_name,
    "replace_restores_name",
    CaseState::Works,
    Some("e2e_restore_reverts_modified_object_to_backup_state"),
    "Name rollback covered for simple object."
);
case_fn!(
    replace_restores_property_fields,
    "replace_restores_property_fields",
    CaseState::Works,
    Some("e2e_restore_replace_restores_property_fields"),
    "Executable replace test verifies rollback of task checkbox property."
);
case_fn!(
    replace_restores_body,
    "replace_restores_body",
    CaseState::Works,
    Some("e2e_restore_reverts_modified_object_to_backup_state"),
    "Body rollback covered for simple object."
);
case_fn!(
    replace_restores_last_modified_date,
    "replace_restores_last_modified_date",
    CaseState::Works,
    Some("e2e_restore_reverts_modified_object_to_backup_state"),
    "lastModifiedDate rollback covered for simple object."
);
case_fn!(
    replace_object_type_object,
    "replace_object_type_object",
    CaseState::Works,
    Some("e2e_restore_reverts_modified_object_to_backup_state"),
    "Object replace rollback covered for name/body/lastModifiedDate."
);
case_fn!(
    replace_object_type_file,
    "replace_object_type_file",
    CaseState::Fails,
    Some("e2e_restore_replace_file_object_reverts_name"),
    "Executable replace test fails: file name does not revert to backup value."
);
case_fn!(
    replace_object_type_type,
    "replace_object_type_type",
    CaseState::Works,
    Some("e2e_restore_replace_type_object_reverts_fields"),
    "Executable replace test passes for custom type object rollback."
);
case_fn!(
    replace_object_type_property,
    "replace_object_type_property",
    CaseState::Fails,
    Some("e2e_restore_replace_property_object_reverts_fields"),
    "Executable replace test fails: property object fields do not revert."
);
case_fn!(
    replace_object_type_collection_with_items,
    "replace_object_type_collection_with_items",
    CaseState::Fails,
    Some("e2e_restore_replace_collection_with_items_reverts_membership"),
    "Executable replace test fails: restored collection/list endpoint returns not found for expected collection object."
);
case_fn!(
    replace_object_type_custom_type_object,
    "replace_object_type_custom_type_object",
    CaseState::Works,
    Some("e2e_restore_replace_custom_type_object_reverts_type_and_fields"),
    "Executable replace test passes for custom-type object rollback."
);
case_fn!(
    replace_object_type_complex_nested_object,
    "replace_object_type_complex_nested_object",
    CaseState::Fails,
    Some("e2e_restore_replace_complex_nested_object_reverts_graph"),
    "Executable replace test fails: nested collection/list endpoint returns not found for expected collection object."
);
case_fn!(
    replace_after_object_type_changed_since_backup,
    "replace_after_object_type_changed_since_backup",
    CaseState::Works,
    Some("e2e_restore_replace_after_object_type_changed_since_backup"),
    "Executable replace test passes when object type changed after backup."
);
case_fn!(
    restore_full_then_two_incrementals_into_different_space,
    "restore_full_then_two_incrementals_into_different_space",
    CaseState::Works,
    Some("e2e_incremental_restore_chain_applies_sequential_changes"),
    "Incremental chain apply path covered."
);
case_fn!(
    replace_unarchives_object_archived_after_backup,
    "replace_unarchives_object_archived_after_backup",
    CaseState::Works,
    Some("e2e_restore_recovers_deleted_object"),
    "Known fix on server patch path."
);

fn p1_cases() -> Vec<CaseDef> {
    vec![
        restore_non_archived_object_between_spaces_preserves_fields(),
        restore_non_archived_image_between_spaces_preserves_fields(),
        restore_non_archived_pdf_between_spaces_preserves_fields(),
        restore_type_object_between_spaces_preserves_fields(),
        restore_property_object_between_spaces_preserves_fields(),
        restore_collection_and_items_between_spaces_preserves_fields(),
        restore_custom_type_object_between_spaces_preserves_fields(),
        restore_full_backup_into_new_space(),
    ]
}

fn p2_cases() -> Vec<CaseDef> {
    vec![
        restore_permanently_deleted_object_same_space(),
        restore_permanently_deleted_file_same_space(),
        restore_permanently_deleted_type_same_space(),
        restore_permanently_deleted_property_same_space(),
        restore_permanently_deleted_collection_with_items_same_space(),
        restore_permanently_deleted_preserves_date_fields(),
        restore_permanently_deleted_object_variant_object(),
        restore_permanently_deleted_object_variant_file(),
        replace_restores_name(),
        replace_restores_property_fields(),
        replace_restores_body(),
        replace_restores_last_modified_date(),
        replace_object_type_object(),
        replace_object_type_file(),
        replace_object_type_type(),
        replace_object_type_property(),
        replace_object_type_collection_with_items(),
        replace_object_type_custom_type_object(),
        replace_object_type_complex_nested_object(),
        replace_after_object_type_changed_since_backup(),
        restore_full_then_two_incrementals_into_different_space(),
        replace_unarchives_object_archived_after_backup(),
    ]
}

fn print_group_report(group: &str, cases: &[CaseDef]) {
    eprintln!("=== {} ===", group);
    for case in cases {
        let mapped = case.mapped_test.unwrap_or("-");
        eprintln!(
            "{} state={:?} mapped={} note={}",
            case.name, case.state, mapped, case.note
        );
    }
    let works = cases.iter().filter(|c| c.state == CaseState::Works).count();
    let fails = cases.iter().filter(|c| c.state == CaseState::Fails).count();
    let untested = cases
        .iter()
        .filter(|c| c.state == CaseState::Untested)
        .count();
    eprintln!(
        "{} summary: works={} fails={} untested={} total={}",
        group,
        works,
        fails,
        untested,
        cases.len()
    );
}

fn assert_case_registered(case: CaseDef) {
    assert!(
        !case.name.trim().is_empty(),
        "case must have a non-empty name"
    );
    if let Some(mapped) = case.mapped_test {
        assert!(
            !mapped.trim().is_empty(),
            "mapped test name cannot be empty"
        );
    }
}

#[test]
fn group1() {
    let cases = p1_cases();
    print_group_report("group1", &cases);
    assert!(!cases.is_empty(), "group1 must have cases");
}

#[test]
fn group2() {
    let cases = p2_cases();
    print_group_report("group2", &cases);
    assert!(!cases.is_empty(), "group2 must have cases");
}

// One test function per bullet/sub-bullet case for fine-grained mapping.
#[test]
fn case_restore_non_archived_object_between_spaces_preserves_fields() {
    assert_case_registered(restore_non_archived_object_between_spaces_preserves_fields());
}
#[test]
fn case_restore_non_archived_image_between_spaces_preserves_fields() {
    assert_case_registered(restore_non_archived_image_between_spaces_preserves_fields());
}
#[test]
fn case_restore_non_archived_pdf_between_spaces_preserves_fields() {
    assert_case_registered(restore_non_archived_pdf_between_spaces_preserves_fields());
}
#[test]
fn case_restore_type_object_between_spaces_preserves_fields() {
    assert_case_registered(restore_type_object_between_spaces_preserves_fields());
}
#[test]
fn case_restore_property_object_between_spaces_preserves_fields() {
    assert_case_registered(restore_property_object_between_spaces_preserves_fields());
}
#[test]
fn case_restore_collection_and_items_between_spaces_preserves_fields() {
    assert_case_registered(restore_collection_and_items_between_spaces_preserves_fields());
}
#[test]
fn case_restore_custom_type_object_between_spaces_preserves_fields() {
    assert_case_registered(restore_custom_type_object_between_spaces_preserves_fields());
}
#[test]
fn case_restore_full_backup_into_new_space() {
    assert_case_registered(restore_full_backup_into_new_space());
}
#[test]
fn case_restore_permanently_deleted_object_same_space() {
    assert_case_registered(restore_permanently_deleted_object_same_space());
}
#[test]
fn case_restore_permanently_deleted_file_same_space() {
    assert_case_registered(restore_permanently_deleted_file_same_space());
}
#[test]
fn case_restore_permanently_deleted_type_same_space() {
    assert_case_registered(restore_permanently_deleted_type_same_space());
}
#[test]
fn case_restore_permanently_deleted_property_same_space() {
    assert_case_registered(restore_permanently_deleted_property_same_space());
}
#[test]
fn case_restore_permanently_deleted_collection_with_items_same_space() {
    assert_case_registered(restore_permanently_deleted_collection_with_items_same_space());
}
#[test]
fn case_restore_permanently_deleted_preserves_date_fields() {
    assert_case_registered(restore_permanently_deleted_preserves_date_fields());
}
#[test]
fn case_restore_permanently_deleted_object_variant_object() {
    assert_case_registered(restore_permanently_deleted_object_variant_object());
}
#[test]
fn case_restore_permanently_deleted_object_variant_file() {
    assert_case_registered(restore_permanently_deleted_object_variant_file());
}
#[test]
fn case_replace_restores_name() {
    assert_case_registered(replace_restores_name());
}
#[test]
fn case_replace_restores_property_fields() {
    assert_case_registered(replace_restores_property_fields());
}
#[test]
fn case_replace_restores_body() {
    assert_case_registered(replace_restores_body());
}
#[test]
fn case_replace_restores_last_modified_date() {
    assert_case_registered(replace_restores_last_modified_date());
}
#[test]
fn case_replace_object_type_object() {
    assert_case_registered(replace_object_type_object());
}
#[test]
fn case_replace_object_type_file() {
    assert_case_registered(replace_object_type_file());
}
#[test]
fn case_replace_object_type_type() {
    assert_case_registered(replace_object_type_type());
}
#[test]
fn case_replace_object_type_type_has_expected_mapping() {
    let case = replace_object_type_type();
    assert_eq!(
        case.state,
        CaseState::Works,
        "type-object replace case should be marked working"
    );
    assert_eq!(
        case.mapped_test,
        Some("e2e_restore_replace_type_object_reverts_fields"),
        "type-object replace case should map to executable coverage"
    );
}
#[test]
fn case_replace_object_type_property() {
    assert_case_registered(replace_object_type_property());
}
#[test]
fn case_replace_object_type_collection_with_items() {
    assert_case_registered(replace_object_type_collection_with_items());
}
#[test]
fn case_replace_object_type_custom_type_object() {
    assert_case_registered(replace_object_type_custom_type_object());
}
#[test]
fn case_replace_object_type_complex_nested_object() {
    assert_case_registered(replace_object_type_complex_nested_object());
}
#[test]
fn case_replace_after_object_type_changed_since_backup() {
    assert_case_registered(replace_after_object_type_changed_since_backup());
}
#[test]
fn case_restore_full_then_two_incrementals_into_different_space() {
    assert_case_registered(restore_full_then_two_incrementals_into_different_space());
}
#[test]
fn case_replace_unarchives_object_archived_after_backup() {
    assert_case_registered(replace_unarchives_object_archived_after_backup());
}
