use std::collections::BTreeSet;

use anytype::{
    prelude::*,
    test_util::{TestError, TestResult, unique_suffix},
};
use tokio::time::{Duration, Instant, sleep};

const INVENTORY_LIMIT: usize = 100;
const CONVERGENCE_TIMEOUT: Duration = Duration::from_secs(30);
const CONVERGENCE_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReactionInventory {
    emoji: String,
    identities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MessageInventory {
    id: String,
    reactions: Vec<ReactionInventory>,
    read: bool,
    mention_read: bool,
    unread_reaction: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatInventory {
    id: String,
    space_id: String,
    messages_unread: i32,
    mentions_unread: i32,
    messages: Vec<MessageInventory>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountInventory {
    chats: Vec<ChatInventory>,
}

#[derive(Debug, PartialEq, Eq)]
struct AccountContentInventory {
    chats: Vec<ChatContentInventory>,
}

#[derive(Debug, PartialEq, Eq)]
struct ChatContentInventory {
    id: String,
    space_id: String,
    messages: Vec<MessageContentInventory>,
}

#[derive(Debug, PartialEq, Eq)]
struct MessageContentInventory {
    id: String,
    reactions: Vec<ReactionInventory>,
}

fn assertion(message: impl Into<String>) -> TestError {
    TestError::Assertion {
        message: message.into(),
    }
}

impl AccountInventory {
    fn content(&self) -> AccountContentInventory {
        AccountContentInventory {
            chats: self
                .chats
                .iter()
                .map(|chat| ChatContentInventory {
                    id: chat.id.clone(),
                    space_id: chat.space_id.clone(),
                    messages: chat
                        .messages
                        .iter()
                        .map(|message| MessageContentInventory {
                            id: message.id.clone(),
                            reactions: message.reactions.clone(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn globally_read(&self) -> bool {
        self.chats.iter().all(|chat| {
            chat.messages_unread == 0
                && chat.mentions_unread == 0
                && chat
                    .messages
                    .iter()
                    .all(|message| message.read && !message.unread_reaction)
        })
    }
}

async fn account_inventory(client: &AnytypeClient) -> TestResult<AccountInventory> {
    client.cache().clear_spaces();
    let mut chat_objects = client
        .chats()
        .list_chats()
        .limit(INVENTORY_LIMIT as u32)
        .list()
        .await?
        .items;
    if chat_objects.len() >= INVENTORY_LIMIT {
        return Err(assertion(
            "account chat inventory reached its completeness bound",
        ));
    }
    chat_objects.sort_by(|left, right| left.id.cmp(&right.id));
    if chat_objects.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(assertion("account chat inventory contained duplicate ids"));
    }

    let mut chats = Vec::with_capacity(chat_objects.len());
    for chat in chat_objects {
        let page = client
            .chats()
            .list_messages(&chat.id)
            .limit(INVENTORY_LIMIT)
            .list_page()
            .await?;
        if page.messages.len() >= INVENTORY_LIMIT {
            return Err(assertion(
                "account message inventory reached its completeness bound",
            ));
        }
        let mut messages = page
            .messages
            .into_iter()
            .map(|message| {
                let mut reactions = message
                    .reactions
                    .into_iter()
                    .map(|reaction| {
                        let mut identities = reaction.identities;
                        identities.sort();
                        ReactionInventory {
                            emoji: reaction.emoji,
                            identities,
                        }
                    })
                    .collect::<Vec<_>>();
                reactions.sort_by(|left, right| {
                    left.emoji
                        .cmp(&right.emoji)
                        .then_with(|| left.identities.cmp(&right.identities))
                });
                MessageInventory {
                    id: message.id,
                    reactions,
                    read: message.read,
                    mention_read: message.mention_read,
                    unread_reaction: message.unread_reaction,
                }
            })
            .collect::<Vec<_>>();
        messages.sort_by(|left, right| left.id.cmp(&right.id));
        if messages.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(assertion(
                "account message inventory contained duplicate ids",
            ));
        }
        chats.push(ChatInventory {
            id: chat.id,
            space_id: chat.space_id,
            messages_unread: page.state.messages_unread,
            mentions_unread: page.state.mentions_unread,
            messages,
        });
    }
    Ok(AccountInventory { chats })
}

async fn wait_for_unread_fixture(
    client: &AnytypeClient,
    expected_chat_ids: &BTreeSet<String>,
) -> TestResult<AccountInventory> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let inventory = account_inventory(client).await?;
        let actual_chat_ids = inventory
            .chats
            .iter()
            .map(|chat| chat.id.clone())
            .collect::<BTreeSet<_>>();
        let ready = actual_chat_ids == *expected_chat_ids
            && inventory.chats.iter().all(|chat| {
                chat.messages_unread > 0
                    && chat.messages.len() == 1
                    && !chat.messages[0].read
                    && chat.messages[0].reactions.len() == 1
            });
        if ready {
            return Ok(inventory);
        }
        if Instant::now() >= deadline {
            return Err(assertion(format!(
                "complete account fixture did not converge to unread state: {inventory:?}",
            )));
        }
        sleep(CONVERGENCE_DELAY).await;
    }
}

async fn wait_for_global_read(
    client: &AnytypeClient,
    before: &AccountInventory,
) -> TestResult<AccountInventory> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        let inventory = account_inventory(client).await?;
        if inventory.content() == before.content() && inventory.globally_read() {
            return Ok(inventory);
        }
        if Instant::now() >= deadline {
            return Err(assertion("account-global chat read state did not converge"));
        }
        sleep(CONVERGENCE_DELAY).await;
    }
}

async fn wait_for_inventory(client: &AnytypeClient, expected: &AccountInventory) -> TestResult<()> {
    let deadline = Instant::now() + CONVERGENCE_TIMEOUT;
    loop {
        if account_inventory(client).await? == *expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(assertion(
                "account inventory did not return to its exact baseline",
            ));
        }
        sleep(CONVERGENCE_DELAY).await;
    }
}

async fn exercise_global_read(
    client: &AnytypeClient,
    owned_space_ids: &mut Vec<String>,
) -> TestResult<()> {
    let baseline = account_inventory(client).await?;
    if !baseline.chats.is_empty() {
        return Err(assertion(
            "account-global chat test requires a fresh account with no chats",
        ));
    }

    let suffix = unique_suffix();
    let mut chat_ids = BTreeSet::new();
    for index in 0..2 {
        let space = client
            .new_space(format!("account-global-read-{index}-{suffix}"))
            .create()
            .await?;
        owned_space_ids.push(space.id.clone());
        let chat = client
            .chats()
            .in_space(&space.id)
            .create(
                format!("account-global-chat-{index}-{suffix}"),
                Icon::Emoji {
                    emoji: "✅".to_owned(),
                },
            )
            .create()
            .await?;
        let message_id = client
            .chats()
            .send_text(&chat.id, format!("account-global fixture {index}"))
            .send()
            .await?;
        client
            .chats()
            .toggle_reaction(&chat.id, &message_id, "✅")
            .send()
            .await?;
        client
            .chats()
            .unread_messages(&chat.id)
            .mark_unread()
            .await?;
        if !chat_ids.insert(chat.id) {
            return Err(assertion("chat-space fixture returned a duplicate chat id"));
        }
    }

    let before = wait_for_unread_fixture(client, &chat_ids).await?;
    client.chats().read_all_account().mark_read().await?;
    let after = wait_for_global_read(client, &before).await?;
    if after.chats.len() != 2 {
        return Err(assertion(
            "account-global read evidence did not cover both fixture chats",
        ));
    }
    Ok(())
}

async fn cleanup_owned_spaces(
    client: &AnytypeClient,
    owned_space_ids: &[String],
    baseline: &AccountInventory,
) -> TestResult<()> {
    for space_id in owned_space_ids.iter().rev() {
        let _ = client.delete_space(space_id).await;
    }
    // Exact account inventory is authoritative even when a delete response is
    // ambiguous: cleanup succeeds only after every owned chat has disappeared.
    wait_for_inventory(client, baseline).await
}

#[tokio::test]
#[ignore = "requires a dedicated disposable account and owned server process tree"]
#[serial_test::serial(disposable_anytype_api)]
async fn account_global_chat_read_all_converges_and_preserves_inventory() -> TestResult<()> {
    if std::env::var("ANYTYPE_ACCOUNT_GLOBAL_TEST_PROCESS").as_deref() != Ok("1") {
        return Err(assertion(
            "account-global chat test lacks dedicated-process admission",
        ));
    }
    let client = AnytypeClient::new("anytype_account_global_chat_test")?;
    let baseline = account_inventory(&client).await?;
    let mut owned_space_ids = Vec::new();
    let exercise = exercise_global_read(&client, &mut owned_space_ids).await;
    let cleanup = cleanup_owned_spaces(&client, &owned_space_ids, &baseline).await;
    match (exercise, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
