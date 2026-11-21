//! Entity attributes (`Attributes`, `AttributeInstance`, `RangedAttribute`):
//! base values, modifiers and vanilla's exact evaluation order.

/// Every vanilla attribute, in registry order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Attr {
    Armor,
    ArmorToughness,
    AttackDamage,
    AttackKnockback,
    AttackSpeed,
    BlockBreakSpeed,
    BlockInteractionRange,
    BurningTime,
    CameraDistance,
    ExplosionKnockbackResistance,
    EntityInteractionRange,
    FallDamageMultiplier,
    FlyingSpeed,
    FollowRange,
    Gravity,
    JumpStrength,
    KnockbackResistance,
    Luck,
    MaxAbsorption,
    MaxHealth,
    MiningEfficiency,
    MovementEfficiency,
    MovementSpeed,
    OxygenBonus,
    SafeFallDistance,
    Scale,
    SneakingSpeed,
    SpawnReinforcements,
    StepHeight,
    SubmergedMiningSpeed,
    SweepingDamageRatio,
    TemptRange,
    WaterMovementEfficiency,
    WaypointTransmitRange,
    WaypointReceiveRange,
}

pub const COUNT: usize = 35;

/// (registry name, default, min, max, synced to the client)
const DEFS: [(&str, f64, f64, f64, bool); COUNT] = [
    ("armor", 0.0, 0.0, 30.0, true),
    ("armor_toughness", 0.0, 0.0, 20.0, true),
    ("attack_damage", 2.0, 0.0, 2048.0, false),
    ("attack_knockback", 0.0, 0.0, 5.0, false),
    ("attack_speed", 4.0, 0.0, 1024.0, true),
    ("block_break_speed", 1.0, 0.0, 1024.0, true),
    ("block_interaction_range", 4.5, 0.0, 64.0, true),
    ("burning_time", 1.0, 0.0, 1024.0, true),
    ("camera_distance", 4.0, 0.0, 32.0, true),
    ("explosion_knockback_resistance", 0.0, 0.0, 1.0, true),
    ("entity_interaction_range", 3.0, 0.0, 64.0, true),
    ("fall_damage_multiplier", 1.0, 0.0, 100.0, true),
    ("flying_speed", 0.4, 0.0, 1024.0, true),
    ("follow_range", 32.0, 0.0, 2048.0, false),
    ("gravity", 0.08, -1.0, 1.0, true),
    ("jump_strength", 0.42f32 as f64, 0.0, 32.0, true),
    ("knockback_resistance", 0.0, 0.0, 1.0, false),
    ("luck", 0.0, -1024.0, 1024.0, true),
    ("max_absorption", 0.0, 0.0, 2048.0, true),
    ("max_health", 20.0, 1.0, 1024.0, true),
    ("mining_efficiency", 0.0, 0.0, 1024.0, true),
    ("movement_efficiency", 0.0, 0.0, 1.0, true),
    ("movement_speed", 0.7, 0.0, 1024.0, true),
    ("oxygen_bonus", 0.0, 0.0, 1024.0, true),
    ("safe_fall_distance", 3.0, -1024.0, 1024.0, true),
    ("scale", 1.0, 0.0625, 16.0, true),
    ("sneaking_speed", 0.3, 0.0, 1.0, true),
    ("spawn_reinforcements", 0.0, 0.0, 1.0, false),
    ("step_height", 0.6, 0.0, 10.0, true),
    ("submerged_mining_speed", 0.2, 0.0, 20.0, true),
    ("sweeping_damage_ratio", 0.0, 0.0, 1.0, true),
    ("tempt_range", 10.0, 0.0, 2048.0, false),
    ("water_movement_efficiency", 0.0, 0.0, 1.0, true),
    ("waypoint_transmit_range", 0.0, 0.0, 6.0e7, false),
    ("waypoint_receive_range", 0.0, 0.0, 6.0e7, false),
];

pub const ALL: [Attr; COUNT] = {
    let mut a = [Attr::Armor; COUNT];
    let mut i = 0;
    while i < COUNT {
        // SAFETY: Attr is repr(u8) with COUNT contiguous variants starting at 0
        a[i] = unsafe { std::mem::transmute::<u8, Attr>(i as u8) };
        i += 1;
    }
    a
};

impl Attr {
    pub fn name(self) -> &'static str {
        DEFS[self as usize].0
    }
    pub fn default_value(self) -> f64 {
        DEFS[self as usize].1
    }
    pub fn synced(self) -> bool {
        DEFS[self as usize].4
    }
    /// `RangedAttribute.sanitizeValue`
    pub fn sanitize(self, v: f64) -> f64 {
        let (_, _, min, max, _) = DEFS[self as usize];
        if v.is_nan() {
            min
        } else {
            v.clamp(min, max)
        }
    }
    pub fn from_name(n: &str) -> Option<Attr> {
        let n = n.trim_start_matches("minecraft:");
        ALL.into_iter().find(|a| a.name() == n)
    }
    /// protocol id (registry order)
    pub fn id(self) -> i32 {
        self as i32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    AddValue,
    AddMultipliedBase,
    AddMultipliedTotal,
}

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        match s {
            "add_value" => Some(Op::AddValue),
            "add_multiplied_base" => Some(Op::AddMultipliedBase),
            "add_multiplied_total" => Some(Op::AddMultipliedTotal),
            _ => None,
        }
    }
    pub fn id(self) -> i32 {
        self as i32
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Modifier {
    /// resource location, e.g. `minecraft:base_attack_damage`
    pub id: String,
    pub amount: f64,
    pub op: Op,
}

/// `AttributeInstance`: base value plus modifiers keyed by id (insertion
/// ordered, like vanilla's `Object2ObjectArrayMap`).
#[derive(Clone, Debug)]
pub struct Instance {
    pub base: f64,
    pub modifiers: Vec<Modifier>,
    cached: Option<f64>,
}

impl Instance {
    pub fn new(base: f64) -> Self {
        Self { base, modifiers: Vec::new(), cached: None }
    }

    pub fn add(&mut self, m: Modifier) {
        if let Some(o) = self.modifiers.iter_mut().find(|o| o.id == m.id) {
            *o = m;
        } else {
            self.modifiers.push(m);
        }
        self.cached = None;
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let n = self.modifiers.len();
        self.modifiers.retain(|m| m.id != id);
        self.cached = None;
        n != self.modifiers.len()
    }

    pub fn has(&self, id: &str) -> bool {
        self.modifiers.iter().any(|m| m.id == id)
    }

    pub fn set_base(&mut self, v: f64) {
        if self.base != v {
            self.base = v;
            self.cached = None;
        }
    }

    /// `AttributeInstance.calculateValue`
    pub fn value(&mut self, a: Attr) -> f64 {
        if let Some(v) = self.cached {
            return v;
        }
        let v = calculate(a, self.base, &self.modifiers);
        self.cached = Some(v);
        v
    }
}

pub fn calculate(a: Attr, base_value: f64, mods: &[Modifier]) -> f64 {
    let mut base = base_value;
    for m in mods.iter().filter(|m| m.op == Op::AddValue) {
        base += m.amount;
    }
    let mut result = base;
    for m in mods.iter().filter(|m| m.op == Op::AddMultipliedBase) {
        result += base * m.amount;
    }
    for m in mods.iter().filter(|m| m.op == Op::AddMultipliedTotal) {
        result *= 1.0 + m.amount;
    }
    a.sanitize(result)
}

/// `AttributeMap`: only the attributes the entity type supports exist.
#[derive(Clone, Debug, Default)]
pub struct Map {
    inst: Vec<(Attr, Instance)>,
    /// attributes changed since the last sync (for `update_attributes`)
    pub dirty: Vec<Attr>,
}

impl Map {
    /// From an `AttributeSupplier` list: (attribute, base override).
    pub fn new(supplied: &[(Attr, Option<f64>)]) -> Self {
        Self { inst: supplied.iter().map(|(a, b)| (*a, Instance::new(b.unwrap_or(a.default_value())))).collect(), dirty: Vec::new() }
    }

    pub fn has(&self, a: Attr) -> bool {
        self.inst.iter().any(|(x, _)| *x == a)
    }

    pub fn get_mut(&mut self, a: Attr) -> Option<&mut Instance> {
        self.inst.iter_mut().find(|(x, _)| *x == a).map(|(_, i)| i)
    }

    pub fn get(&self, a: Attr) -> Option<&Instance> {
        self.inst.iter().find(|(x, _)| *x == a).map(|(_, i)| i)
    }

    /// `AttributeMap.getValue` (an unsupported attribute reads as its
    /// default, which vanilla would throw on; callers only ask for
    /// supported ones).
    pub fn value(&mut self, a: Attr) -> f64 {
        match self.get_mut(a) {
            Some(i) => i.value(a),
            None => a.default_value(),
        }
    }

    pub fn base(&self, a: Attr) -> f64 {
        self.get(a).map(|i| i.base).unwrap_or(a.default_value())
    }

    pub fn set_base(&mut self, a: Attr, v: f64) {
        if let Some(i) = self.get_mut(a) {
            i.set_base(v);
            self.mark(a);
        }
    }

    pub fn add_modifier(&mut self, a: Attr, m: Modifier) {
        if let Some(i) = self.get_mut(a) {
            i.add(m);
            self.mark(a);
        }
    }

    pub fn remove_modifier(&mut self, a: Attr, id: &str) -> bool {
        let removed = self.get_mut(a).is_some_and(|i| i.remove(id));
        if removed {
            self.mark(a);
        }
        removed
    }

    fn mark(&mut self, a: Attr) {
        if !self.dirty.contains(&a) {
            self.dirty.push(a);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Attr, &Instance)> {
        self.inst.iter().map(|(a, i)| (*a, i))
    }
}

/// `LivingEntity.createLivingAttributes`
pub fn living() -> Vec<(Attr, Option<f64>)> {
    use Attr::*;
    [
        MaxHealth,
        KnockbackResistance,
        MovementSpeed,
        Armor,
        ArmorToughness,
        MaxAbsorption,
        StepHeight,
        Scale,
        Gravity,
        SafeFallDistance,
        FallDamageMultiplier,
        JumpStrength,
        EntityInteractionRange,
        OxygenBonus,
        BurningTime,
        ExplosionKnockbackResistance,
        WaterMovementEfficiency,
        MovementEfficiency,
        AttackKnockback,
        CameraDistance,
        WaypointTransmitRange,
    ]
    .into_iter()
    .map(|a| (a, None))
    .collect()
}

/// `Player.createAttributes`
pub fn player() -> Vec<(Attr, Option<f64>)> {
    use Attr::*;
    let mut v = living();
    let mut set = |a: Attr, b: Option<f64>| match v.iter_mut().find(|(x, _)| *x == a) {
        Some(e) => e.1 = b,
        None => v.push((a, b)),
    };
    set(AttackDamage, Some(1.0));
    set(MovementSpeed, Some(0.1f32 as f64));
    set(AttackSpeed, None);
    set(Luck, None);
    set(BlockInteractionRange, None);
    set(BlockBreakSpeed, None);
    set(SubmergedMiningSpeed, None);
    set(SneakingSpeed, None);
    set(MiningEfficiency, None);
    set(SweepingDamageRatio, None);
    set(WaypointTransmitRange, Some(6.0e7));
    set(WaypointReceiveRange, Some(6.0e7));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_and_clamp() {
        let mods = vec![
            Modifier { id: "a".into(), amount: 2.0, op: Op::AddValue },
            Modifier { id: "b".into(), amount: 0.5, op: Op::AddMultipliedBase },
            Modifier { id: "c".into(), amount: 1.0, op: Op::AddMultipliedTotal },
        ];
        // (4 + 2) = 6; + 6*0.5 = 9; * 2 = 18
        assert_eq!(calculate(Attr::AttackSpeed, 4.0, &mods), 18.0);
        assert_eq!(calculate(Attr::Armor, 50.0, &[]), 30.0);
        assert_eq!(calculate(Attr::MaxHealth, f64::NAN, &[]), 1.0);
        assert_eq!(ALL[Attr::WaypointReceiveRange as usize], Attr::WaypointReceiveRange);
        assert_eq!(Attr::from_name("minecraft:attack_speed"), Some(Attr::AttackSpeed));
    }
}
