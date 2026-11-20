//! Skin conversion between Java and Bedrock, fixing the classic cross-play
//! skin bugs (cf. GeyserSkinManager):
//!
//! * **Legacy 64×32 Java skins** show missing/garbled left arm and leg on
//!   Bedrock → upgraded to 64×64 by mirroring the right limbs, exactly like the
//!   vanilla Java client does.
//! * **Bedrock HD skins (128×128)** are invalid on Java → box-filtered down to 64×64.
//! * **Semi-transparent base layers** (common on Bedrock) render black or
//!   see-through on Java → base layer alpha forced opaque; overlay layer keeps alpha.
//! * **Slim (Alex) arms** are carried across via the geometry name / model flag,
//!   so arms don't render with a 1-pixel gap.
//! * **Malformed skin payloads** (wrong length, absurd sizes) are rejected
//!   instead of crashing or being forwarded.

pub const GEOMETRY_CLASSIC: &str = "geometry.humanoid.custom";
pub const GEOMETRY_SLIM: &str = "geometry.humanoid.customSlim";

#[derive(Clone, Debug, PartialEq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Option<Self> {
        let allowed = matches!((width, height), (64, 32) | (64, 64) | (128, 64) | (128, 128));
        if !allowed || data.len() != (width * height * 4) as usize {
            return None;
        }
        Some(Self { width, height, data })
    }
    fn px(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        self.data[i..i + 4].try_into().unwrap()
    }
    fn set(&mut self, x: u32, y: u32, p: [u8; 4]) {
        let i = ((y * self.width + x) * 4) as usize;
        self.data[i..i + 4].copy_from_slice(&p);
    }
}

/// Copy a rectangle, optionally mirrored horizontally.
fn blit(img: &mut Rgba, sx: u32, sy: u32, w: u32, h: u32, dx: u32, dy: u32, mirror: bool) {
    for y in 0..h {
        for x in 0..w {
            let p = img.px(sx + x, sy + y);
            let tx = if mirror { dx + (w - 1 - x) } else { dx + x };
            img.set(tx, dy + y, p);
        }
    }
}

/// Upgrade a legacy 64×32 skin to 64×64 like the vanilla client does.
pub fn upgrade_legacy(img: &Rgba) -> Rgba {
    if img.height != 32 {
        return img.clone();
    }
    let mut out = Rgba { width: 64, height: 64, data: vec![0; 64 * 64 * 4] };
    out.data[..64 * 32 * 4].copy_from_slice(&img.data);
    // (src_x, src_y, w, h, dst_x, dst_y) — vanilla's mirror table for the left limbs
    let parts = [
        (4, 16, 4, 4, 20, 48),
        (8, 16, 4, 4, 24, 48),
        (0, 20, 4, 12, 24, 52),
        (4, 20, 4, 12, 20, 52),
        (8, 20, 4, 12, 16, 52),
        (12, 20, 4, 12, 28, 52),
        (44, 16, 4, 4, 36, 48),
        (48, 16, 4, 4, 40, 48),
        (40, 20, 4, 12, 40, 52),
        (44, 20, 4, 12, 36, 52),
        (48, 20, 4, 12, 32, 52),
        (52, 20, 4, 12, 44, 52),
    ];
    for (sx, sy, w, h, dx, dy) in parts {
        blit(&mut out, sx, sy, w, h, dx, dy, true);
    }
    out
}

/// Base-layer regions of a 64×64 skin (head, body, arms, legs — no overlays).
const BASE_REGIONS: [(u32, u32, u32, u32); 6] = [(0, 0, 32, 16), (16, 16, 24, 16), (40, 16, 16, 16), (0, 16, 16, 16), (16, 48, 16, 16), (32, 48, 16, 16)];

/// Force the base layer opaque (Java renders translucent base pixels wrongly).
pub fn fix_transparency(img: &mut Rgba) {
    if img.width != 64 || img.height != 64 {
        return;
    }
    for (x0, y0, w, h) in BASE_REGIONS {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let mut p = img.px(x, y);
                if p[3] != 0 {
                    p[3] = 255;
                    img.set(x, y, p);
                }
            }
        }
    }
}

/// 2×2 box filter (128 → 64). Alpha-weighted so edges don't turn dark.
fn downscale_half(img: &Rgba) -> Rgba {
    let (w, h) = (img.width / 2, img.height / 2);
    let mut out = Rgba { width: w, height: h, data: vec![0; (w * h * 4) as usize] };
    for y in 0..h {
        for x in 0..w {
            let ps = [img.px(2 * x, 2 * y), img.px(2 * x + 1, 2 * y), img.px(2 * x, 2 * y + 1), img.px(2 * x + 1, 2 * y + 1)];
            let a: u32 = ps.iter().map(|p| p[3] as u32).sum();
            let mut px = [0u8; 4];
            if a > 0 {
                for c in 0..3 {
                    px[c] = (ps.iter().map(|p| p[c] as u32 * p[3] as u32).sum::<u32>() / a) as u8;
                }
            }
            px[3] = (a / 4) as u8;
            out.set(x, y, px);
        }
    }
    out
}

pub struct BedrockSkin {
    pub skin_id: String,
    pub image: Rgba,
    pub geometry_name: &'static str,
    pub resource_patch: String,
}

/// Java → Bedrock.
pub fn java_to_bedrock(img: &Rgba, slim: bool, skin_hash: &str) -> BedrockSkin {
    let mut image = upgrade_legacy(img);
    fix_transparency(&mut image);
    let geometry_name = if slim { GEOMETRY_SLIM } else { GEOMETRY_CLASSIC };
    BedrockSkin {
        skin_id: format!("rime.{}", skin_hash.chars().filter(|c| c.is_ascii_alphanumeric()).take(64).collect::<String>()),
        image,
        geometry_name,
        resource_patch: serde_json::json!({ "geometry": { "default": geometry_name } }).to_string(),
    }
}

/// Bedrock → Java (64×64 RGBA + slim flag). Custom (persona/3D) geometry falls
/// back to the classic model so the texture still maps onto the Java player.
pub fn bedrock_to_java(img: &Rgba, geometry_name: &str) -> (Rgba, bool) {
    let mut out = match (img.width, img.height) {
        (128, 128) => downscale_half(img),
        (128, 64) => upgrade_legacy(&downscale_half(img)),
        (64, 32) => upgrade_legacy(img),
        _ => img.clone(),
    };
    fix_transparency(&mut out);
    let slim = geometry_name.to_ascii_lowercase().contains("slim");
    (out, slim)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, p: [u8; 4]) -> Rgba {
        Rgba::new(w, h, p.iter().copied().cycle().take((w * h * 4) as usize).collect()).unwrap()
    }

    #[test]
    fn rejects_bad_payloads() {
        assert!(Rgba::new(64, 64, vec![0; 10]).is_none());
        assert!(Rgba::new(4096, 4096, vec![]).is_none());
    }

    #[test]
    fn legacy_upgrade_fills_left_limbs() {
        let mut img = solid(64, 32, [0, 0, 0, 0]);
        // paint right leg front (4..8, 20..32) red
        for y in 20..32 {
            for x in 4..8 {
                img.set(x, y, [255, 0, 0, 255]);
            }
        }
        let up = upgrade_legacy(&img);
        assert_eq!((up.width, up.height), (64, 64));
        // left leg front lives at (20..24, 52..64)
        assert_eq!(up.px(20, 52), [255, 0, 0, 255]);
        assert_eq!(up.px(23, 63), [255, 0, 0, 255]);
    }

    #[test]
    fn hd_and_transparency() {
        let img = solid(128, 128, [10, 20, 30, 128]);
        let (j, slim) = bedrock_to_java(&img, "geometry.humanoid.customSlim");
        assert!(slim);
        assert_eq!((j.width, j.height), (64, 64));
        assert_eq!(j.px(8, 8)[3], 255); // head base layer made opaque
        assert_eq!(j.px(40, 8)[3], 128); // hat overlay keeps its alpha
    }

    #[test]
    fn java_to_bedrock_geometry() {
        let s = java_to_bedrock(&solid(64, 64, [1, 2, 3, 255]), true, "ab/../cd");
        assert_eq!(s.geometry_name, GEOMETRY_SLIM);
        assert_eq!(s.skin_id, "rime.abcd");
        assert!(s.resource_patch.contains("customSlim"));
    }
}
