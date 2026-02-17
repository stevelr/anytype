#![allow(dead_code)]

use std::{process::Command, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use anytype::prelude::*;
use serde_json::Value;
use tokio::time::sleep;

#[derive(Debug, Clone)]
pub struct GeneratedObject {
    pub id: String,
}

#[derive(Debug, Clone)]
pub struct GeneratedFixture {
    pub prefix: String,
    pub objects: Vec<GeneratedObject>,
}

pub async fn generate_fixture(
    source_space_name: &str,
    source_space_id: &str,
) -> Result<GeneratedFixture> {
    let prefix = format!("anyback-e2e-{}", anytype::test_util::unique_suffix());
    let mut objects = Vec::new();

    // 1) basic page
    let page1_name = format!("{prefix} page-1");
    let page1 = create_object_with_retry(
        source_space_name,
        "page",
        &page1_name,
        Some("# E2E Fixture\n\nBase page"),
    )
    .await?;
    objects.push(GeneratedObject { id: page1.clone() });

    // 2) note
    let note_name = format!("{prefix} note-1");
    let note =
        create_object_with_retry(source_space_name, "note", &note_name, Some("note body")).await?;
    objects.push(GeneratedObject { id: note.clone() });

    // 3) task
    let task_name = format!("{prefix} task-1");
    let task =
        create_object_with_retry(source_space_name, "task", &task_name, Some("task body")).await?;
    objects.push(GeneratedObject { id: task.clone() });

    // 4) page with references to earlier objects
    let page2_name = format!("{prefix} page-2-linked");
    let page2_body = format!(
        "# Linked fixture\n\n- object id ref: {}\n- object link: https://object.any.coop/{}?spaceId={}\n",
        page1, note, source_space_id
    );
    let page2 =
        create_object_with_retry(source_space_name, "page", &page2_name, Some(&page2_body)).await?;
    objects.push(GeneratedObject { id: page2 });

    // 5) another page
    let page3_name = format!("{prefix} page-3");
    let page3 =
        create_object_with_retry(source_space_name, "page", &page3_name, Some("# page 3")).await?;
    objects.push(GeneratedObject { id: page3 });

    // 6) another note
    let note2_name = format!("{prefix} note-2");
    let note2 =
        create_object_with_retry(source_space_name, "note", &note2_name, Some("note 2 body"))
            .await?;
    objects.push(GeneratedObject { id: note2 });

    Ok(GeneratedFixture { prefix, objects })
}

async fn create_object_with_retry(
    space_name: &str,
    type_key: &str,
    name: &str,
    body: Option<&str>,
) -> Result<String> {
    let mut delay_ms = 200u64;
    for attempt in 1..=8 {
        match create_object_once(space_name, type_key, name, body) {
            Ok(id) => return Ok(id),
            Err(err) => {
                let text = err.to_string();
                let retryable = text.contains("internal_server_error")
                    || text.contains("failed to create object")
                    || text.contains("status\":500");

                if retryable && attempt < 8 {
                    eprintln!(
                        "retrying anyr object create {type_key} '{name}' attempt {attempt}: {err}"
                    );
                    sleep(Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(2500);
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(anyhow!("exhausted retries creating object {name}"))
}

fn create_object_once(
    space_name: &str,
    type_key: &str,
    name: &str,
    body: Option<&str>,
) -> Result<String> {
    let mut cmd = Command::new("anyr");
    cmd.args(["object", "create", space_name, type_key, "--name", name]);
    if let Some(body) = body {
        cmd.args(["--body", body]);
    }

    let output = cmd
        .output()
        .context("failed to execute anyr object create")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "anyr object create failed (status={}): stdout={} stderr={}",
            output.status,
            stdout,
            stderr
        );
    }

    let value: Value = serde_json::from_slice(&output.stdout)
        .context("failed to parse anyr object create JSON output")?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("anyr create output missing id field"))?;
    Ok(id.to_string())
}

pub async fn cleanup_by_ids(client: &AnytypeClient, space_id: &str, ids: &[String]) -> Result<()> {
    for id in ids {
        let _ = client.object(space_id, id).delete().await;
    }
    Ok(())
}

pub async fn cleanup_by_name_prefix(
    client: &AnytypeClient,
    space_id: &str,
    prefix: &str,
) -> Result<Vec<String>> {
    let objects = client
        .objects(space_id)
        .filter(Filter::text_contains("name", prefix))
        .list()
        .await?
        .collect_all()
        .await?;

    let mut deleted = Vec::new();
    for obj in objects {
        if obj
            .name
            .as_deref()
            .is_some_and(|name| name.contains(prefix))
        {
            let _ = client.object(space_id, &obj.id).delete().await;
            deleted.push(obj.id);
        }
    }
    Ok(deleted)
}
