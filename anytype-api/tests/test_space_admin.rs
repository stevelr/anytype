use anytype::{
    prelude::*,
    test_util::{TestError, TestResult, unique_suffix, with_test_context},
};

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

        ctx.client.enable_space_sharing(&space.id).await?;

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

        let listed = ctx.client.list_space_invites(&space.id).await?;
        assert!(listed.iter().any(|invite| {
            invite.invite_type == "member"
                && invite.permissions.as_deref() == Some("writer")
                && invite.cid == member.cid
        }));

        ctx.client.revoke_space_invite(&space.id).await?;

        let auto_approve = ctx
            .client
            .create_space_invite(
                &space.id,
                SpaceInviteType::AutoApprove,
                SpaceInvitePermission::Owner,
            )
            .await?;
        assert_eq!(auto_approve.invite_type, "auto-approve");
        assert_eq!(auto_approve.permissions.as_deref(), Some("owner"));
        ctx.client.revoke_space_invite(&space.id).await?;

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

        let listed_guest = ctx.client.list_space_invites(&space.id).await?;
        let guest_from_list = listed_guest
            .iter()
            .find(|invite| invite.invite_type == "guest")
            .ok_or_else(|| TestError::Assertion {
                message: "guest invitation was not returned by list_space_invites".to_owned(),
            })?;
        assert_eq!(guest_from_list.cid, guest.cid);
        assert!(guest_from_list.permissions.is_none());

        ctx.client.revoke_space_invite(&space.id).await?;
        ctx.client.disable_space_sharing(&space.id).await?;

        // The space was registered with the test context, so this explicit
        // delete also exercises the public deletion method while teardown can
        // safely prove that the already-absent fixture needs no second write.
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
    let spaces = client.spaces().list().await?.collect_all().await?;
    let space = spaces
        .into_iter()
        .find(|space| space.id == space_id)
        .ok_or_else(|| TestError::Assertion {
            message: "created chat space was not returned by spaces()".to_owned(),
        })?;
    if space.name != expected_name || space.object != SpaceModel::Chat {
        return Err(TestError::Assertion {
            message: "created chat space identity did not round-trip".to_owned(),
        });
    }
    Ok(())
}
