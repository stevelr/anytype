use anyhow::Error;
use anytype::prelude::AnytypeError;

pub fn exit_code(err: &Error) -> i32 {
    if matches!(
        err.downcast_ref::<AnytypeError>(),
        Some(
            AnytypeError::Unauthorized
                | AnytypeError::NoKeyStore
                | AnytypeError::KeyStore { .. }
                | AnytypeError::Auth { .. }
        )
    ) {
        return 2;
    }
    1
}
