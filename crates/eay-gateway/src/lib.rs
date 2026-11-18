//! eay-gateway's HTTP layer: the OpenAI-compatible `/v1` API and the `/gateway/admin` API,
//! on top of `eay_gateway_core`.

pub mod error;
pub mod routes;

pub use eay_gateway_core as core;
pub use error::GatewayError;
pub use routes::gateway_router;
