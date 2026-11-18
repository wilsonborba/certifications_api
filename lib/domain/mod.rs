pub mod auth;
pub mod cleanup;
pub mod generations;
pub mod guardrail;
pub mod quotas;
pub mod security;
pub mod studies;

use crate::{
    core::Settings,
    dal::{Store, scanner::SignatureState},
};

#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub settings: Settings,
    pub quotas: quotas::QuotaCache,
    pub signatures: SignatureState,
}
