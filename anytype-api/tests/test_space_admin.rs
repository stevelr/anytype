use anytype::{
    prelude::*,
    test_util::{TestError, TestResult, unique_suffix, with_test_context},
};
use tokio::time::{Duration, sleep};

const READBACK_ATTEMPTS: usize = 60;
const READBACK_DELAY: Duration = Duration::from_millis(500);

#[tokio::test]
async fn space_administration_validates_before_transport() {
    let mut config = ClientConfig::default().app_name("space-admin-validation");
    config.base_url = Some("http://127.0.0.1:1".to_owned());
    config.keystore = Some("env".to_owned());
    let client = AnytypeClient::with_config(config).expect("validation test client");

    assert!(matches!(
        client.create_chat_space("").await,
        Err(AnytypeError::Validation { .. })
    ));
    assert!(matches!(
        client.delete_space(" ").await,
        Err(AnytypeError::Validation { .. })
    ));
    assert!(matches!(
        client.list_space_invites(" ").await,
        Err(AnytypeError::Validation { .. })
    ));
    assert!(matches!(
        client
            .create_space_invite(" ", SpaceInviteType::Member, SpaceInvitePermission::Reader)
            .await,
        Err(AnytypeError::Validation { .. })
    ));
    assert!(matches!(
        client.revoke_space_invite(" ").await,
        Err(AnytypeError::Validation { .. })
    ));
    assert!(matches!(
        client.enable_space_sharing(" ").await,
        Err(AnytypeError::Validation { .. })
    ));
    assert!(matches!(
        client.disable_space_sharing(" ").await,
        Err(AnytypeError::Validation { .. })
    ));
    assert_eq!(client.http_metrics().logical_operations, 0);
}

#[tokio::test]
#[ignore = "requires configured real server and gRPC credentials"]
#[serial_test::serial(disposable_anytype_api)]
async fn space_administration_lifecycle() -> TestResult<()> {
    with_test_context(|ctx| async move {
        let chat_name = format!("space-admin-chat-{}", unique_suffix());
        let chat = ctx.client.create_chat_space(&chat_name).await?;
        let chat_id = chat.id.clone();
        let chat_check = verify_chat_space(&ctx.client, &chat_id, &chat_name).await;
        let chat_delete = ctx.client.delete_space(&chat_id).await;
        chat_check?;
        chat_delete?;

        let space_name = format!("space-admin-{}", unique_suffix());
        let space = ctx.create_space_fixture(space_name).await?;

        // REST creation returns before the gRPC ACL service always admits the
        // new space on fresh servers.
        sleep(Duration::from_secs(2)).await;
        eprintln!("space administration phase: enable sharing");
        ctx.client.enable_space_sharing(&space.id).await?;

        eprintln!("space administration phase: member invitation");
        let member = ctx
            .client
            .create_space_invite(
                &space.id,
                SpaceInviteType::Member,
                SpaceInvitePermission::Writer,
            )
            .await?;
        assert_eq!(member.invite_type, "member");
        assert_eq!(member.permissions.as_deref(), Some("writer"));
        assert!(member.url.starts_with("https://invite.any.coop/"));

        // Heart versions differ in how the current-invite readback maps ACL
        // permission tiers. The create response above owns requested-value
        // verification; inventory readback owns stable type and CID identity.
        wait_for_space_invite(&ctx.client, &space.id, "member", &member.cid).await?;

        eprintln!("space administration phase: revoke member invitation");
        ctx.client.revoke_space_invite(&space.id).await?;
        wait_for_space_invite_absent(&ctx.client, &space.id, &member.cid).await?;

        eprintln!("space administration phase: auto-approve invitation");
        let auto_approve = ctx
            .client
            .create_space_invite(
                &space.id,
                SpaceInviteType::AutoApprove,
                SpaceInvitePermission::Reader,
            )
            .await?;
        assert_eq!(auto_approve.invite_type, "auto-approve");
        assert_eq!(auto_approve.permissions.as_deref(), Some("reader"));
        eprintln!("space administration phase: revoke auto-approve invitation");
        ctx.client.revoke_space_invite(&space.id).await?;
        wait_for_space_invite_absent(&ctx.client, &space.id, &auto_approve.cid).await?;

        eprintln!("space administration phase: guest invitation");
        let guest = ctx
            .client
            .create_space_invite(
                &space.id,
                SpaceInviteType::Guest,
                SpaceInvitePermission::Reader,
            )
            .await?;
        assert_eq!(guest.invite_type, "guest");
        assert_eq!(guest.permissions.as_deref(), Some("reader"));

        let guest_from_list =
            wait_for_space_invite(&ctx.client, &space.id, "guest", &guest.cid).await?;
        assert_eq!(guest_from_list.cid, guest.cid);
        // A guest returned through the current-invite command retains its
        // reader tier; the dedicated guest command omits permissions.
        assert!(matches!(
            guest_from_list.permissions.as_deref(),
            None | Some("reader")
        ));

        eprintln!("space administration phase: revoke guest invitation");
        ctx.client.revoke_space_invite(&space.id).await?;
        wait_for_space_invite_absent(&ctx.client, &space.id, &guest.cid).await?;
        eprintln!("space administration phase: disable sharing");
        ctx.client.disable_space_sharing(&space.id).await?;

        // The space was registered with the test context, so this explicit
        // delete also exercises the public deletion method while teardown can
        // safely prove that the already-absent fixture needs no second write.
        eprintln!("space administration phase: delete space");
        ctx.client.delete_space(&space.id).await?;
        Ok(())
    })
    .await
}

async fn verify_chat_space(
    client: &AnytypeClient,
    space_id: &str,
    expected_name: &str,
) -> TestResult<()> {
    let mut saw_id = false;
    let mut saw_name = false;
    let mut saw_compatible_model = false;
    for attempt in 0..READBACK_ATTEMPTS {
        let spaces = client.spaces().list().await?.collect_all().await?;
        for space in spaces {
            if space.id == space_id {
                saw_id = true;
                saw_name |= space.name == expected_name;
                // Heart 0.50.10 reports the immutable regular space type even
                // when the separate UX detail selects chat. API versions that
                // expose that UX as the model still report `chat`.
                let compatible_model = matches!(space.object, SpaceModel::Space | SpaceModel::Chat);
                saw_compatible_model |= compatible_model;
                if space.name == expected_name && compatible_model {
                    return Ok(());
                }
            }
        }
        if attempt + 1 < READBACK_ATTEMPTS {
            sleep(READBACK_DELAY).await;
        }
    }
    let category = match (saw_id, saw_name, saw_compatible_model) {
        (false, _, _) => "missing",
        (true, false, false) => "name_and_model",
        (true, false, true) => "name",
        (true, true, false) => "model",
        (true, true, true) => "unstable",
    };
    Err(TestError::Assertion {
        message: format!("created chat space identity did not converge: {category}"),
    })
}

async fn wait_for_space_invite(
    client: &AnytypeClient,
    space_id: &str,
    expected_type: &str,
    expected_cid: &str,
) -> TestResult<SpaceInvite> {
    let mut saw_cid = false;
    let mut saw_type = false;
    for attempt in 0..READBACK_ATTEMPTS {
        let invites = client.list_space_invites(space_id).await?;
        for invite in &invites {
            saw_cid |= invite.cid == expected_cid;
            saw_type |= invite.invite_type == expected_type;
        }
        if let Some(invite) = invites
            .into_iter()
            .find(|invite| invite.invite_type == expected_type && invite.cid == expected_cid)
        {
            return Ok(invite);
        }
        if attempt + 1 < READBACK_ATTEMPTS {
            sleep(READBACK_DELAY).await;
        }
    }
    let category = match (saw_cid, saw_type) {
        (false, false) => "missing",
        (false, true) => "cid",
        (true, false) => "type",
        (true, true) => "unstable",
    };
    Err(TestError::Assertion {
        message: format!("created space invitation did not converge: {category}"),
    })
}

async fn wait_for_space_invite_absent(
    client: &AnytypeClient,
    space_id: &str,
    revoked_cid: &str,
) -> TestResult<()> {
    for attempt in 0..READBACK_ATTEMPTS {
        let invites = client.list_space_invites(space_id).await?;
        if invites.iter().all(|invite| invite.cid != revoked_cid) {
            // The invitation details clear before the ACL record necessarily
            // settles. A short fixed margin prevents the next replacement
            // from racing that background transition.
            sleep(Duration::from_secs(2)).await;
            return Ok(());
        }
        if attempt + 1 < READBACK_ATTEMPTS {
            sleep(READBACK_DELAY).await;
        }
    }
    Err(TestError::Assertion {
        message: "revoked space invitation remained visible".to_owned(),
    })
}
