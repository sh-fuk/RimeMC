//! Official vanilla data (world generation, tags, registry contents).
//!
//! Mojang's data is not redistributed with RimeMC. On first start the
//! official server jar is downloaded from Mojang (or taken from a local file),
//! its SHA-1 is verified against the value Mojang publishes, and only the
//! `data/` directory is extracted into `cache/<version>/data`.

use std::fs;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context};
use sha1::{Digest, Sha1};

use crate::registry::blocks::MC_VERSION;

pub const SERVER_JAR_URL: &str = "https://piston-data.mojang.com/v1/objects/3872a7f07a1a595e651aef8b058dfc2bb3772f46/server.jar";
pub const SERVER_JAR_SHA1: &str = "3872a7f07a1a595e651aef8b058dfc2bb3772f46";
const MAX_JAR: u64 = 200 * 1024 * 1024;
const MAX_ENTRY: u64 = 64 * 1024 * 1024;

pub struct VanillaData {
    pub root: PathBuf,
}

fn sha1_hex(b: &[u8]) -> String {
    hex::encode(Sha1::digest(b))
}

fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let mut resp = ureq::get(url).call().with_context(|| format!("downloading {url}"))?;
    let body = resp.body_mut().with_config().limit(MAX_JAR).read_to_vec()?;
    Ok(body)
}

/// Only plain relative paths under `data/` are accepted (no `..`, no absolute paths).
fn safe_rel(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if !name.starts_with("data/") || name.ends_with('/') {
        return None;
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            _ => return None,
        }
    }
    Some(out)
}

impl VanillaData {
    /// Make sure the data exists in `<server_dir>/cache/<version>`; download if needed.
    pub fn ensure(server_dir: &Path) -> anyhow::Result<Self> {
        let root = server_dir.join("cache").join(MC_VERSION);
        let marker = root.join("data").join(".complete");
        if marker.exists() {
            return Ok(Self { root });
        }
        fs::create_dir_all(&root)?;
        let jar_path = root.join("server.jar");
        let jar = match fs::read(&jar_path) {
            Ok(b) if sha1_hex(&b) == SERVER_JAR_SHA1 => b,
            _ => {
                tracing::info!("first start: downloading official Minecraft {MC_VERSION} data from Mojang (~60 MB, once)…");
                let b = download(SERVER_JAR_URL)?;
                let got = sha1_hex(&b);
                if got != SERVER_JAR_SHA1 {
                    bail!("downloaded server jar has SHA-1 {got}, expected {SERVER_JAR_SHA1}; refusing to use it");
                }
                fs::write(&jar_path, &b)?;
                b
            }
        };
        Self::extract(&jar, &root)?;
        fs::write(&marker, b"ok")?;
        tracing::info!("vanilla data ready in {}", root.display());
        Ok(Self { root })
    }

    fn extract(jar: &[u8], root: &Path) -> anyhow::Result<()> {
        let mut outer = zip::ZipArchive::new(Cursor::new(jar))?;
        let inner_name = format!("META-INF/versions/{MC_VERSION}/server-{MC_VERSION}.jar");
        let mut inner_bytes = Vec::new();
        outer.by_name(&inner_name).with_context(|| format!("{inner_name} not found in server jar"))?.take(MAX_JAR).read_to_end(&mut inner_bytes)?;
        let mut inner = zip::ZipArchive::new(Cursor::new(inner_bytes))?;
        let tmp = root.join("data.tmp");
        let _ = fs::remove_dir_all(&tmp);
        let mut n = 0;
        for i in 0..inner.len() {
            let mut f = inner.by_index(i)?;
            let Ok(name) = f.name().map(|n| n.into_owned()) else { continue };
            let Some(rel) = safe_rel(&name) else { continue };
            if f.size() > MAX_ENTRY {
                bail!("suspicious entry size in server jar: {name}");
            }
            let dest = tmp.join(rel.strip_prefix("data").unwrap_or(&rel));
            if let Some(d) = dest.parent() {
                fs::create_dir_all(d)?;
            }
            let mut buf = Vec::with_capacity(f.size() as usize);
            (&mut f).take(MAX_ENTRY).read_to_end(&mut buf)?;
            fs::write(&dest, buf)?;
            n += 1;
        }
        let final_dir = root.join("data");
        let _ = fs::remove_dir_all(&final_dir);
        fs::rename(&tmp, &final_dir)?;
        tracing::info!("extracted {n} vanilla data files");
        Ok(())
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join("data").join(rel)
    }

    pub fn read_json(&self, rel: &str) -> anyhow::Result<serde_json::Value> {
        let p = self.path(rel);
        let text = fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    /// All JSON entries of a registry directory as (`minecraft:<key>`, path),
    /// sorted by key (the order vanilla uses for data-driven registries).
    pub fn entries(&self, dir: &str) -> Vec<(String, PathBuf)> {
        let base = self.path(&format!("minecraft/{dir}"));
        let mut out = Vec::new();
        fn walk(base: &Path, cur: &Path, out: &mut Vec<(String, PathBuf)>) {
            let Ok(rd) = fs::read_dir(cur) else { return };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(base, &p, out);
                } else if p.extension().is_some_and(|x| x == "json") {
                    let rel = p.strip_prefix(base).unwrap().with_extension("");
                    let key = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
                    out.push((format!("minecraft:{key}"), p));
                }
            }
        }
        walk(&base, &base, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_traversal_rejected() {
        assert!(safe_rel("data/minecraft/worldgen/noise/x.json").is_some());
        assert!(safe_rel("data/../../etc/passwd").is_none());
        assert!(safe_rel("/data/x").is_none());
        assert!(safe_rel("assets/x.json").is_none());
        assert!(safe_rel("data/minecraft/").is_none());
    }
}
