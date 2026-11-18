pub mod auth;
pub mod guardrail;
pub mod security;

use crate::{core::Settings, dal::Store};

#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub settings: Settings,
}
