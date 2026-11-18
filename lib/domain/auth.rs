use crate::core::{Error, Result};
use serde_json::Value;

pub const ADMIN_ACCESS_LEVEL: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerIdentity {
    pub user_id: String,
    pub access_level: i64,
    pub display_name: Option<String>,
}

impl CallerIdentity {
    pub fn is_admin(&self) -> bool {
        self.access_level == ADMIN_ACCESS_LEVEL
    }
}

pub fn require_owner(caller: &CallerIdentity, resource_owner_id: &str) -> Result<()> {
    if caller.user_id != resource_owner_id {
        return Err(Error::forbidden(
            "forbidden: Caller does not own this resource",
        ));
    }
    Ok(())
}

pub fn require_owner_or_admin(caller: &CallerIdentity, resource_owner_id: &str) -> Result<()> {
    if caller.user_id != resource_owner_id && !caller.is_admin() {
        return Err(Error::forbidden(
            "forbidden: Caller does not own this resource",
        ));
    }
    Ok(())
}

pub fn require_admin(access_level: Option<i64>) -> Result<()> {
    if access_level != Some(ADMIN_ACCESS_LEVEL) {
        return Err(Error::forbidden(
            "admin_required: Administrator access required",
        ));
    }
    Ok(())
}

pub fn reject_caller_authority_fields(payload: &Value) -> Result<()> {
    if let Some(obj) = payload.as_object() {
        for forbidden in ["owner_id", "user_id", "access_level", "is_admin", "role"] {
            if obj.contains_key(forbidden) {
                return Err(Error::unprocessable(format!(
                    "authority_field_forbidden: Caller payload cannot set '{forbidden}'"
                )));
            }
        }
    }
    Ok(())
}
