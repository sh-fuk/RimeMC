//! Vanilla combat rules: attributes, damage types and sources, enchantments,
//! mob effects and the `LivingEntity` damage pipeline.

pub mod attr;
pub mod damage;
pub mod effects;
pub mod enchant;
pub mod living;

/// Loads the data-driven parts (damage types, enchantments, tags).
pub fn load(v: &crate::vanilla::VanillaData) -> anyhow::Result<()> {
    damage::DamageTypes::load(v)?;
    enchant::Enchantments::load(v)?;
    crate::game::weather::load(v)?;
    Ok(())
}
