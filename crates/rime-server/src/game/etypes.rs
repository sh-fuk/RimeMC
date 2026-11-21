//! Entity type table (`assets/26.1/entities.tsv`, dumped from vanilla
//! `EntityType` + `DefaultAttributes`): size, category, fire immunity,
//! tracking range and the exact default attributes of every type.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::combat::attr::{self, Attr};

#[derive(Clone, Debug)]
pub struct EntityTypeInfo {
    pub name: &'static str,
    /// protocol id (registry order)
    pub id: i32,
    pub category: &'static str,
    pub width: f64,
    pub height: f64,
    pub eye_height: f64,
    pub fire_immune: bool,
    pub tracking_range: i32,
    pub update_interval: i32,
    /// default attribute supplier (empty for non-living entities)
    pub attrs: Vec<(Attr, f64)>,
}

pub struct EntityTypes {
    pub list: Vec<EntityTypeInfo>,
    by_name: HashMap<&'static str, usize>,
}

impl EntityTypes {
    pub fn get() -> &'static EntityTypes {
        static T: OnceLock<EntityTypes> = OnceLock::new();
        T.get_or_init(|| {
            let mut list = Vec::new();
            for (id, line) in include_str!("../../assets/26.1/entities.tsv").lines().filter(|l| !l.is_empty()).enumerate() {
                let f: Vec<&'static str> = line.split('\t').collect();
                let num = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                let attrs = f
                    .get(8)
                    .map(|s| {
                        s.split(',')
                            .filter_map(|kv| {
                                let (k, v) = kv.split_once('=')?;
                                Some((Attr::from_name(k)?, v.parse().ok()?))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                list.push(EntityTypeInfo {
                    name: f[0],
                    id: id as i32,
                    category: f.get(1).copied().unwrap_or("misc"),
                    width: num(2),
                    height: num(3),
                    eye_height: num(4),
                    fire_immune: f.get(5) == Some(&"true"),
                    tracking_range: num(6) as i32,
                    update_interval: num(7) as i32,
                    attrs,
                });
            }
            let by_name = list.iter().enumerate().map(|(i, t)| (t.name, i)).collect();
            EntityTypes { list, by_name }
        })
    }

    pub fn by_name(&self, name: &str) -> Option<&EntityTypeInfo> {
        self.by_name.get(name.trim_start_matches("minecraft:")).map(|&i| &self.list[i])
    }

    /// The static name of a type (for `EntInfo`).
    pub fn static_name(name: &str) -> &'static str {
        EntityTypes::get().by_name(name).map(|t| t.name).unwrap_or("pig")
    }
}

impl EntityTypeInfo {
    /// A fresh attribute map with this type's defaults.
    pub fn attributes(&self) -> attr::Map {
        let list: Vec<(Attr, Option<f64>)> = self.attrs.iter().map(|&(a, v)| (a, Some(v))).collect();
        attr::Map::new(&list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        let t = EntityTypes::get();
        let z = t.by_name("zombie").unwrap();
        assert_eq!((z.width, z.height), (0.6, 1.95));
        let mut m = z.attributes();
        assert_eq!(m.value(Attr::Armor), 2.0);
        assert_eq!(m.value(Attr::FollowRange), 35.0);
        assert_eq!(m.value(Attr::MovementSpeed), 0.23f32 as f64);
        assert!(t.by_name("blaze").unwrap().fire_immune);
        assert_eq!(t.by_name("player").unwrap().attrs.len(), 31);
    }
}
