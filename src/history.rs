//! Historique des modifications (libellés lisibles) et versions nommées d'une photo.

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::adjust::Adjustments;

/// Version nommée d'une photo, gardée dans son fichier .luminux.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub name: String,
    pub adjustments: Adjustments,
}

fn label_id() -> egui::Id {
    egui::Id::unique("luminux_derniere_modif")
}

/// Retient le libellé de la modification en cours (curseur déplacé, bouton…).
pub fn mark(ctx: &egui::Context, label: String) {
    ctx.data_mut(|d| d.insert_temp(label_id(), label));
}

/// Libellé retenu depuis la dernière entrée d'historique, effacé au passage.
pub fn take(ctx: &egui::Context) -> Option<String> {
    ctx.data_mut(|d| d.remove_temp::<String>(label_id()))
}

/// Nom lisible d'un champ des réglages.
fn field_label(k: &str) -> &'static str {
    match k {
        "exposure" => t!("Exposition"),
        "temperature" => t!("Température"),
        "tint" => t!("Teinte"),
        "contrast" => t!("Contraste"),
        "highlights" => t!("Hautes lumières"),
        "shadows" => t!("Ombres"),
        "whites" => t!("Blancs"),
        "blacks" => t!("Noirs"),
        "saturation" => t!("Saturation"),
        "vibrance" => t!("Vibrance"),
        "curve" => t!("Courbe des tons"),
        "hsl" => t!("TSL"),
        "crop" => t!("Recadrage"),
        "angle" => t!("Redressement"),
        "sharpen" => t!("Netteté"),
        "nr_luma" | "nr_color" => t!("Réduction du bruit"),
        "vignette" | "vignette_mid" => t!("Vignettage"),
        "clarity" => t!("Clarté"),
        "texture" => t!("Texture"),
        k if k.starts_with("split_") => t!("Virage partiel"),
        k if k.starts_with("balance_") => t!("Balance des couleurs"),
        "grain" | "grain_size" => t!("Grain"),
        "lut" | "lut_amount" => t!("Looks"),
        "locals" => t!("Masques"),
        "sky" => t!("Ciel"),
        "lens" => t!("Objectif"),
        "retouch" => t!("Retouche"),
        "depth" => t!("Profondeur"),
        "enhance" => t!("Amélioration IA"),
        "portrait" => t!("Portrait"),
        "denoise" => t!("Débruitage IA"),
        _ => t!("Réglages"),
    }
}

/// Ce qui a changé entre deux états, pour une entrée d'historique sans libellé retenu.
pub fn describe(before: &Adjustments, after: &Adjustments) -> String {
    let (Ok(serde_json::Value::Object(a)), Ok(serde_json::Value::Object(b))) = (serde_json::to_value(before), serde_json::to_value(after)) else {
        return t!("Réglages").into();
    };
    let mut names: Vec<&str> = Vec::new();
    for (k, v) in &b {
        if a.get(k) != Some(v) {
            let n = field_label(k);
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    match names.len() {
        0 => t!("Réglages").into(),
        1..=3 => names.join(", "),
        n => tf!("{0} réglages", n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_changed_fields() {
        let a = Adjustments::default();
        let mut b = a.clone();
        b.exposure = 0.5;
        b.nr_luma = 0.2;
        b.nr_color = 0.2;
        // Indépendant de la langue (un autre test la change).
        assert_eq!(describe(&a, &b).split(", ").count(), 2);
        b.contrast = 0.1;
        b.clarity = 0.1;
        assert!(describe(&a, &b).starts_with("4 "));
    }
}
