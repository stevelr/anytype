// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Live (tier-2) evidence for exact exported-Markdown replacement fidelity.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anytype::{
    body::{BlockContent, BodySnapshot},
    test_util::{
        DisposableRun, TestContext, TestError, TestResult, unique_suffix,
        with_disposable_space_context,
    },
};

const STABILITY_ATTEMPTS: usize = 12;
const STABILITY_DELAY: Duration = Duration::from_millis(100);

struct FidelityCase {
    label: &'static str,
    markdown: &'static str,
    expected_kinds: &'static [&'static str],
    expected_contract: FidelityContract,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FidelityContract {
    ByteIdentity,
    TypedSemanticStability,
    UnsupportedDrift,
}

impl FidelityContract {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ByteIdentity => "byte_identity",
            Self::TypedSemanticStability => "typed_semantic_stability",
            Self::UnsupportedDrift => "unsupported_drift",
        }
    }
}

const CASES: &[FidelityCase] = &[
    FidelityCase {
        label: "headings",
        markdown: "# Heading one\n\n## Heading two\n\nBody text.",
        expected_kinds: &["text:Header2"],
        expected_contract: FidelityContract::ByteIdentity,
    },
    FidelityCase {
        label: "lists",
        markdown: "- bullet one\n- bullet two\n\n1. numbered one\n2. numbered two",
        expected_kinds: &["text:Bulleted", "text:Numbered"],
        expected_contract: FidelityContract::ByteIdentity,
    },
    FidelityCase {
        label: "checkboxes",
        markdown: "- [ ] unchecked\n- [x] checked",
        expected_kinds: &["text:Checkbox"],
        expected_contract: FidelityContract::ByteIdentity,
    },
    FidelityCase {
        label: "quotes",
        markdown: "> quoted paragraph",
        expected_kinds: &["text:Quote"],
        expected_contract: FidelityContract::ByteIdentity,
    },
    FidelityCase {
        label: "multiline-quotes",
        markdown: "> quoted first line\n> quoted second line",
        expected_kinds: &["text:Quote"],
        expected_contract: FidelityContract::UnsupportedDrift,
    },
    FidelityCase {
        label: "fenced-code",
        markdown: "```rust\nfn main() {\n    println!(\"hello\");\n}\n```",
        expected_kinds: &["text:Code"],
        expected_contract: FidelityContract::UnsupportedDrift,
    },
    FidelityCase {
        label: "links",
        markdown: "A [bounded link](https://example.com/path?q=one) in a paragraph.",
        expected_kinds: &["text:Paragraph"],
        expected_contract: FidelityContract::ByteIdentity,
    },
    FidelityCase {
        label: "tables",
        markdown: "| left | right |\n| --- | --- |\n| alpha | beta |\n| gamma | delta |",
        expected_kinds: &["table"],
        expected_contract: FidelityContract::UnsupportedDrift,
    },
    FidelityCase {
        label: "unicode",
        markdown: "こんにちは 👋 — café naïve Ελληνικά 中文",
        expected_kinds: &["text:Paragraph"],
        expected_contract: FidelityContract::ByteIdentity,
    },
    FidelityCase {
        label: "underscores",
        markdown: "literal_identifier snake_case and _emphasized text_",
        expected_kinds: &["text:Paragraph"],
        expected_contract: FidelityContract::UnsupportedDrift,
    },
    FidelityCase {
        label: "escapes",
        markdown: r"Escaped \*stars\*, \_underscores\_, and a literal \\ slash.",
        expected_kinds: &["text:Paragraph"],
        expected_contract: FidelityContract::UnsupportedDrift,
    },
    FidelityCase {
        label: "multiline",
        markdown: "First paragraph spans\nmultiple source lines.\n\nSecond paragraph.\n\nThird paragraph.",
        expected_kinds: &["text:Paragraph"],
        expected_contract: FidelityContract::ByteIdentity,
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockEvidence {
    identity: Vec<(String, String, Vec<String>)>,
    semantics: Vec<(String, usize)>,
}

fn assertion(message: &str) -> TestError {
    TestError::Assertion {
        message: message.to_owned(),
    }
}

fn block_kind(content: &BlockContent) -> String {
    match content {
        BlockContent::Text(text) => format!("text:{:?}", text.style),
        BlockContent::Layout(_) => "layout".to_owned(),
        BlockContent::Divider(_) => "divider".to_owned(),
        BlockContent::Bookmark(_) => "bookmark".to_owned(),
        BlockContent::Link(_) => "link".to_owned(),
        BlockContent::Relation(_) => "relation".to_owned(),
        BlockContent::FeaturedRelations => "featured_relations".to_owned(),
        BlockContent::Embed(_) => "embed".to_owned(),
        BlockContent::TableOfContents => "table_of_contents".to_owned(),
        BlockContent::Table => "table".to_owned(),
        BlockContent::TableRow { .. } => "table_row".to_owned(),
        BlockContent::TableColumn => "table_column".to_owned(),
        BlockContent::File(_) => "file".to_owned(),
        BlockContent::Unsupported(_) => "unsupported".to_owned(),
        _ => "future".to_owned(),
    }
}

fn block_evidence(snapshot: &BodySnapshot) -> TestResult<BlockEvidence> {
    let mut identity = Vec::with_capacity(snapshot.len());
    let mut semantics = Vec::with_capacity(snapshot.len());
    for block in snapshot.iter() {
        let kind = block_kind(&block.content);
        let content = serde_json::to_string(&block.content)
            .map_err(|_| assertion("serialize typed block evidence"))?;
        identity.push((
            block.id.as_str().to_owned(),
            kind,
            block
                .children
                .iter()
                .map(|child| child.as_str().to_owned())
                .collect(),
        ));
        semantics.push((content, block.children.len()));
    }
    Ok(BlockEvidence {
        identity,
        semantics,
    })
}

async fn stable_export(ctx: &TestContext, object_id: &str) -> TestResult<String> {
    let mut previous = None;
    for _ in 0..STABILITY_ATTEMPTS {
        let markdown = ctx
            .client
            .object(&ctx.space_id, object_id)
            .get()
            .await?
            .markdown
            .ok_or_else(|| assertion("object export omitted Markdown"))?;
        if previous.as_ref() == Some(&markdown) {
            return Ok(markdown);
        }
        previous = Some(markdown);
        tokio::time::sleep(STABILITY_DELAY).await;
    }
    Err(assertion("Markdown export did not reach two stable reads"))
}

async fn stable_blocks(ctx: &TestContext, object_id: &str) -> TestResult<BlockEvidence> {
    let mut previous = None;
    for _ in 0..STABILITY_ATTEMPTS {
        let snapshot = ctx
            .client
            .blocks()
            .body(&ctx.space_id, object_id)
            .fetch()
            .await?;
        let evidence = block_evidence(&snapshot)?;
        if previous.as_ref() == Some(&evidence) {
            return Ok(evidence);
        }
        previous = Some(evidence);
        tokio::time::sleep(STABILITY_DELAY).await;
    }
    Err(assertion(
        "ObjectShow block identity and order did not reach two stable reads",
    ))
}

fn first_byte_drift(before: &str, after: &str) -> Option<usize> {
    before
        .bytes()
        .zip(after.bytes())
        .position(|(left, right)| left != right)
        .or_else(|| (before.len() != after.len()).then_some(before.len().min(after.len())))
}

#[tokio::test]
#[ignore = "requires configured real server and disposable test admission"]
#[serial_test::serial(disposable_anytype_api)]
async fn exact_exported_markdown_replacement_fidelity_matrix() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "markdown-noop-fidelity",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let mut byte_identity_cases = 0_usize;
                let mut semantic_identity_cases = 0_usize;
                let mut unsupported_drift_cases = 0_usize;
                for case in CASES {
                    let object = ctx
                        .client
                        .new_object(&ctx.space_id, "page")
                        .name(format!("Markdown fidelity {} {}", case.label, unique_suffix()))
                        .body(case.markdown)
                        .create()
                        .await?;
                    ctx.register_object(&object.id);

                    let before_export = stable_export(ctx.as_ref(), &object.id).await?;
                    let before_blocks = stable_blocks(ctx.as_ref(), &object.id).await?;
                    let kinds = before_blocks
                        .identity
                        .iter()
                        .map(|(_, kind, _)| kind.as_str())
                        .collect::<Vec<_>>();
                    for expected in case.expected_kinds {
                        if !kinds.contains(expected) {
                            eprintln!(
                                "markdown fidelity missing kind: case={} expected={} observed={kinds:?}",
                                case.label, expected
                            );
                            return Err(assertion("Markdown case omitted an expected block kind"));
                        }
                    }

                    ctx.client
                        .update_object(&ctx.space_id, &object.id)
                        .body(&before_export)
                        .update()
                        .await?;

                    let after_export = stable_export(ctx.as_ref(), &object.id).await?;
                    let after_blocks = stable_blocks(ctx.as_ref(), &object.id).await?;
                    let byte_identity = before_export == after_export;
                    let semantic_identity = before_blocks.semantics == after_blocks.semantics;
                    let contract = if byte_identity {
                        byte_identity_cases += 1;
                        FidelityContract::ByteIdentity
                    } else if semantic_identity {
                        semantic_identity_cases += 1;
                        FidelityContract::TypedSemanticStability
                    } else {
                        unsupported_drift_cases += 1;
                        FidelityContract::UnsupportedDrift
                    };
                    let block_identity = before_blocks.identity == after_blocks.identity;
                    eprintln!(
                        "markdown fidelity evidence: case={} contract={} before_bytes={} after_bytes={} first_byte_drift={:?} before_blocks={} after_blocks={} block_identity={}",
                        case.label,
                        contract.as_str(),
                        before_export.len(),
                        after_export.len(),
                        first_byte_drift(&before_export, &after_export),
                        before_blocks.identity.len(),
                        after_blocks.identity.len(),
                        block_identity,
                    );
                    if contract != case.expected_contract {
                        return Err(assertion(
                            "Markdown fidelity contract changed from the reviewed matrix",
                        ));
                    }
                }
                if byte_identity_cases == 0 {
                    return Err(assertion("Markdown matrix established no byte-stable case"));
                }
                eprintln!(
                    "markdown fidelity matrix summary: byte_identity={byte_identity_cases} typed_semantic_stability={semantic_identity_cases} unsupported_drift={unsupported_drift_cases}"
                );
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe Markdown fidelity matrix");

    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("Markdown fidelity matrix skipped before callback: {reason:?}");
        }
    }
}
