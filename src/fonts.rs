//! Bundled typefaces. All are licensed under the SIL Open Font License (see `assets/fonts`).

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

const FACES: &[(&str, &[u8])] = &[
    ("literata", include_bytes!("../assets/fonts/Literata.ttf")),
    ("literata-italic", include_bytes!("../assets/fonts/Literata-Italic.ttf")),
    ("garamond", include_bytes!("../assets/fonts/EBGaramond.ttf")),
    ("garamond-italic", include_bytes!("../assets/fonts/EBGaramond-Italic.ttf")),
    ("inter", include_bytes!("../assets/fonts/Inter.ttf")),
    ("inter-italic", include_bytes!("../assets/fonts/Inter-Italic.ttf")),
];

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    let fallbacks = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    for &(name, bytes) in FACES {
        defs.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
        let mut chain = vec![name.to_owned()];
        // Fall back to the roman face of the same family, then egui's defaults.
        if let Some(roman) = name.strip_suffix("-italic") {
            chain.push(roman.to_owned());
        }
        chain.extend(fallbacks.iter().cloned());
        defs.families.insert(FontFamily::Name(name.into()), chain);
    }

    // The interface itself is set in Inter.
    if let Some(prop) = defs.families.get_mut(&FontFamily::Proportional) {
        prop.insert(0, "inter".to_owned());
    }
    ctx.set_fonts(defs);
}

/// Interface text in Inter at a given size.
pub fn ui(size: f32) -> egui::FontId {
    egui::FontId::new(size, FontFamily::Name("inter".into()))
}

/// Display text in Literata.
pub fn serif(size: f32, italic: bool) -> egui::FontId {
    egui::FontId::new(size, FontFamily::Name(if italic { "literata-italic" } else { "literata" }.into()))
}
