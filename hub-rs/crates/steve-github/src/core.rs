//! Applies verified GitHub delivery to the current item cycle.

use async_trait::async_trait;

#[async_trait]
pub trait ItemCore: Send + Sync {
    async fn delivery_verified(&self, proof: steve_types::DeliveryProof) -> Result<(), String>;
}

pub struct NoCore;

#[async_trait]
impl ItemCore for NoCore {
    async fn delivery_verified(&self, _: steve_types::DeliveryProof) -> Result<(), String> {
        Err("item delivery handler unavailable".into())
    }
}
