//! Data sent during the configuration phase: synchronized registries and tags.
//! Built once at startup from the official vanilla data.

use std::collections::{HashMap, HashSet};

use super::codec::{write_string, write_varint};
use crate::nbt::Tag;
use crate::registry::Registries;
use crate::vanilla::VanillaData;

/// Registries the client needs, in the order vanilla sends them.
pub const SYNCED_REGISTRIES: [&str; 28] = [
    "worldgen/biome",
    "chat_type",
    "trim_pattern",
    "trim_material",
    "wolf_variant",
    "wolf_sound_variant",
    "pig_variant",
    "pig_sound_variant",
    "frog_variant",
    "cat_variant",
    "cat_sound_variant",
    "cow_sound_variant",
    "cow_variant",
    "chicken_sound_variant",
    "chicken_variant",
    "zombie_nautilus_variant",
    "painting_variant",
    "dimension_type",
    "damage_type",
    "banner_pattern",
    "enchantment",
    "jukebox_song",
    "instrument",
    "test_environment",
    "test_instance",
    "dialog",
    "world_clock",
    "timeline",
];

/// Registries whose tags are sent to the client.
const TAG_REGISTRIES: [&str; 15] = [
    "dialog",
    "enchantment",
    "entity_type",
    "worldgen/biome",
    "instrument",
    "painting_variant",
    "item",
    "block",
    "fluid",
    "timeline",
    "game_event",
    "potion",
    "damage_type",
    "banner_pattern",
    "point_of_interest_type",
];

pub struct ConfigData {
    /// (registry id, entries (key, network NBT of the value))
    pub registries: Vec<(String, Vec<(String, Vec<u8>)>)>,
    /// body of the `update_tags` packet
    pub tags_body: Vec<u8>,
}

impl ConfigData {
    pub fn build(v: &VanillaData) -> anyhow::Result<Self> {
        let mut registries = Vec::new();
        let mut dynamic_ids: HashMap<String, HashMap<String, i32>> = HashMap::new();
        for reg in SYNCED_REGISTRIES {
            let entries = v.entries(reg);
            anyhow::ensure!(!entries.is_empty(), "vanilla data has no entries for {reg}");
            let mut out = Vec::with_capacity(entries.len());
            let mut ids = HashMap::new();
            for (i, (key, path)) in entries.iter().enumerate() {
                let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
                out.push((key.clone(), Tag::from_json(&json).to_network_bytes()));
                ids.insert(key.clone(), i as i32);
            }
            dynamic_ids.insert(format!("minecraft:{reg}"), ids);
            registries.push((format!("minecraft:{reg}"), out));
        }

        // ---- tags
        let regs = Registries::get();
        let mut body = Vec::new();
        let mut blocks: Vec<(String, Vec<(String, Vec<i32>)>)> = Vec::new();
        for reg in TAG_REGISTRIES {
            let reg_id = format!("minecraft:{reg}");
            let files = v.entries(&format!("tags/{reg}"));
            if files.is_empty() {
                continue;
            }
            let mut raw: HashMap<String, Vec<(String, bool)>> = HashMap::new();
            for (key, path) in &files {
                let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
                let vals = json.get("values").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                let list = vals
                    .iter()
                    .filter_map(|e| match e {
                        serde_json::Value::String(s) => Some((s.clone(), true)),
                        serde_json::Value::Object(o) => Some((o.get("id")?.as_str()?.to_string(), o.get("required").and_then(|r| r.as_bool()).unwrap_or(true))),
                        _ => None,
                    })
                    .collect();
                raw.insert(key.clone(), list);
            }
            let lookup = |name: &str| -> Option<i32> {
                match dynamic_ids.get(&reg_id) {
                    Some(m) => m.get(name).copied(),
                    None => regs.id(&reg_id, name),
                }
            };
            fn resolve(tag: &str, raw: &HashMap<String, Vec<(String, bool)>>, lookup: &dyn Fn(&str) -> Option<i32>, stack: &mut HashSet<String>, out: &mut Vec<i32>) {
                let Some(list) = raw.get(tag) else { return };
                if !stack.insert(tag.to_string()) {
                    return; // cycle
                }
                for (e, _required) in list {
                    if let Some(t) = e.strip_prefix('#') {
                        resolve(t, raw, lookup, stack, out);
                    } else if let Some(id) = lookup(e) {
                        if !out.contains(&id) {
                            out.push(id);
                        }
                    }
                }
                stack.remove(tag);
            }
            let mut tags = Vec::new();
            for (key, _) in &files {
                let mut ids = Vec::new();
                resolve(key, &raw, &lookup, &mut HashSet::new(), &mut ids);
                tags.push((key.clone(), ids));
            }
            blocks.push((reg_id, tags));
        }
        write_varint(&mut body, blocks.len() as i32);
        for (reg, tags) in &blocks {
            write_string(&mut body, reg);
            write_varint(&mut body, tags.len() as i32);
            for (name, ids) in tags {
                write_string(&mut body, name);
                write_varint(&mut body, ids.len() as i32);
                for id in ids {
                    write_varint(&mut body, *id);
                }
            }
        }
        Ok(Self { registries, tags_body: body })
    }

    /// Body of one `registry_data` packet. With `known` the client already has
    /// the vanilla core pack, so only keys are sent (like vanilla does).
    pub fn registry_packet(&self, idx: usize, known: bool) -> Vec<u8> {
        let (id, entries) = &self.registries[idx];
        let mut b = Vec::new();
        write_string(&mut b, id);
        write_varint(&mut b, entries.len() as i32);
        for (k, nbt) in entries {
            write_string(&mut b, k);
            if known {
                b.push(0);
            } else {
                b.push(1);
                b.extend_from_slice(nbt);
            }
        }
        b
    }

    /// Network id of an entry in a synchronized registry.
    pub fn entry_id(&self, registry: &str, name: &str) -> i32 {
        self.registries
            .iter()
            .find(|(r, _)| r == registry)
            .and_then(|(_, e)| e.iter().position(|(k, _)| k == name))
            .unwrap_or(0) as i32
    }
}
