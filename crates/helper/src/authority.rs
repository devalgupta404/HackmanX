//! Asking polkit whether the caller may do something. The GUI runs as the user; only this
//! service runs as root, and it checks every call.

use std::collections::HashMap;

use zbus::fdo;
use zbus::message::Header;
use zbus_polkit::policykit1::{AuthorityProxy, CheckAuthorizationFlags, Subject};

/// Reading disk layouts and running the checks. Allowed for the active local user.
pub const INSPECT: &str = "org.bootshift.inspect";
/// Preparing a change. Always asks for an administrator password, never remembered.
pub const APPLY: &str = "org.bootshift.apply";

pub async fn require(
    connection: &zbus::Connection,
    header: &Header<'_>,
    action: &str,
) -> fdo::Result<()> {
    let subject = Subject::new_for_message_header(header)
        .map_err(|error| fdo::Error::AccessDenied(error.to_string()))?;
    let authority = AuthorityProxy::new(connection).await?;
    let result = authority
        .check_authorization(
            &subject,
            action,
            &HashMap::new(),
            CheckAuthorizationFlags::AllowUserInteraction.into(),
            "",
        )
        .await?;
    if result.is_authorized {
        Ok(())
    } else {
        Err(fdo::Error::AccessDenied(format!(
            "this user isn't allowed to {action}"
        )))
    }
}
