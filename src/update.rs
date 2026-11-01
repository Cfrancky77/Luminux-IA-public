//! Avis de mise à jour : compare la version installée aux releases du dépôt public.
//! Une seule requête GitHub (liste des releases), à la demande ou au plus une fois par jour si
//! l'option est cochée ; rien n'est envoyé sur l'utilisateur ni téléchargé automatiquement.

use anyhow::{Context, Result};
use serde_json::Value;
use std::time::Duration;

/// Dépôt dont les releases sont consultées.
pub const REPO: &str = "Cfrancky77/Luminux-IA-public";

/// Une version plus récente que celle installée.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Étiquette telle que publiée (« v0.35.0 »).
    pub tag: String,
    /// Page de la release (notes et AppImage).
    pub url: String,
}

/// « v1.2.3 », « 1.2.3-beta » ou « 1.2 » -> (1, 2, 3).
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+', ' ']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    let patch = parts.next().map_or(Some(0), |p| p.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// La plus récente des releases publiées (hors brouillons) si elle dépasse `current`.
pub fn newest(releases: &Value, current: &str) -> Option<Release> {
    let current = parse_version(current)?;
    releases
        .as_array()?
        .iter()
        .filter(|r| r["draft"].as_bool() != Some(true))
        .filter_map(|r| {
            let tag = r["tag_name"].as_str()?;
            let url = r["html_url"].as_str()?;
            Some((parse_version(tag)?, Release { tag: tag.to_string(), url: url.to_string() }))
        })
        .max_by_key(|(v, _)| *v)
        .filter(|(v, _)| *v > current)
        .map(|(_, r)| r)
}

/// Interroge GitHub. `Ok(None)` : à jour.
pub fn check(current: &str) -> Result<Option<Release>> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
    // LUMINUX_UPDATE_URL : serveur de test à la place de GitHub.
    let url = std::env::var("LUMINUX_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases?per_page=20"));
    let mut resp = agent
        .get(&url)
        .header("User-Agent", concat!("Luminux-IA/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("consultation des releases")?;
    let text = resp.body_mut().with_config().limit(4 << 20).read_to_string().context("lecture de la réponse")?;
    let json: Value = serde_json::from_str(&text).context("réponse illisible")?;
    Ok(newest(&json, current))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_are_parsed() {
        assert_eq!(parse_version("v0.34.0"), Some((0, 34, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("0.35.1-beta.2"), Some((0, 35, 1)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(parse_version("v0.9.0") < parse_version("v0.10.0"), "comparaison numérique, pas alphabétique");
    }

    #[test]
    fn newest_release_is_picked_and_drafts_ignored() {
        let releases = json!([
            {"tag_name": "v0.34.0", "html_url": "https://x/34", "draft": false, "prerelease": true},
            {"tag_name": "v0.36.0", "html_url": "https://x/36", "draft": true},
            {"tag_name": "v0.35.0", "html_url": "https://x/35", "draft": false, "prerelease": true},
            {"tag_name": "autre", "html_url": "https://x/a"}
        ]);
        assert_eq!(newest(&releases, "0.34.0"), Some(Release { tag: "v0.35.0".into(), url: "https://x/35".into() }));
        assert_eq!(newest(&releases, "0.35.0"), None);
        assert_eq!(newest(&releases, "0.40.0"), None);
        assert_eq!(newest(&json!({"message": "Not Found"}), "0.1.0"), None);
        assert_eq!(newest(&releases, "inconnue"), None);
    }
}
