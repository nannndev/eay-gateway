//! The engine behind eay-gateway: provider catalog, account and key storage, OAuth logins,
//! request routing with fallback, wire-format translators, live quotas and model lists.
//! The HTTP server lives in the `eay-gateway` crate; embed this crate to run routing in-process.

pub mod cipher;
pub mod engine;
pub mod kiro;
pub mod mcp;
pub mod models;
pub mod oauth;
pub mod pricing;
pub mod providers;
pub mod quota;
pub mod store;
pub mod translate;

pub use cipher::Cipher;
pub use engine::{ChatError, ChatReply, ChatStream, GatewayEngine};
pub use providers::{OAuthProviderMeta, Provider, Target, OAUTH_PROVIDERS, PROVIDERS};
pub use store::{AccountRecord, GatewaySettings, GatewayStore, ProviderRecord, VirtualKeyRecord};
