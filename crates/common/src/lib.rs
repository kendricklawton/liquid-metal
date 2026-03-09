pub mod config;
pub mod events;

pub use events::{Engine, EngineSpec, LiquidSpec, MetalSpec, ProvisionEvent};

/// Lowercase, replace non-alphanumeric with `-`, collapse consecutive dashes, trim edges.
/// Used by API and daemon to derive URL-safe slugs from user-supplied names.
pub fn slugify(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !prev_dash { slug.push(c); }
            prev_dash = true;
        } else {
            slug.push(c);
            prev_dash = false;
        }
    }
    slug.trim_matches('-').to_string()
}
