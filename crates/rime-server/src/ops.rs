//! Operator list (`ops.json`, vanilla-compatible shape) and command senders.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

pub enum CommandSender {
    /// server console / control socket: always operator
    Console,
    #[allow(dead_code)]
    Player { name: String },
}

#[derive(Serialize, Deserialize)]
struct OpEntry {
    name: String,
    #[serde(default)]
    uuid: String,
    #[serde(default = "default_level")]
    level: u8,
    #[serde(default, rename = "bypassesPlayerLimit")]
    bypasses_player_limit: bool,
}

fn default_level() -> u8 {
    4
}

pub struct Ops {
    path: PathBuf,
    names: RwLock<HashSet<String>>,
}

impl Ops {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let names = if path.exists() {
            let entries: Vec<OpEntry> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
            entries.into_iter().filter(|e| e.level >= 3).map(|e| e.name.to_ascii_lowercase()).collect()
        } else {
            HashSet::new()
        };
        Ok(Self { path: path.to_path_buf(), names: RwLock::new(names) })
    }

    pub fn is_op(&self, s: &CommandSender) -> bool {
        match s {
            CommandSender::Console => true,
            CommandSender::Player { name } => self.names.read().contains(&name.to_ascii_lowercase()),
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        let mut v: Vec<OpEntry> = self
            .names
            .read()
            .iter()
            .map(|n| OpEntry { name: n.clone(), uuid: String::new(), level: 4, bypasses_player_limit: false })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&v)?)?;
        std::fs::rename(tmp, &self.path)?;
        Ok(())
    }

    pub fn add(&self, name: &str) -> anyhow::Result<()> {
        self.names.write().insert(name.to_ascii_lowercase());
        self.save()
    }

    pub fn remove(&self, name: &str) -> anyhow::Result<()> {
        self.names.write().remove(&name.to_ascii_lowercase());
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_permissions() {
        let p = std::env::temp_dir().join(format!("rime-ops-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let ops = Ops::load(&p).unwrap();
        let steve = CommandSender::Player { name: "Steve".into() };
        assert!(ops.is_op(&CommandSender::Console));
        assert!(!ops.is_op(&steve));
        ops.add("steve").unwrap();
        assert!(Ops::load(&p).unwrap().is_op(&steve));
        ops.remove("Steve").unwrap();
        assert!(!Ops::load(&p).unwrap().is_op(&steve));
        std::fs::remove_file(&p).unwrap();
    }
}
