//! Reading themes, typefaces and the user's typographic preferences.

use eframe::egui::{self, Color32, FontFamily, FontId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeKind {
    Paper,
    Sepia,
    Dusk,
    Night,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 4] = [ThemeKind::Paper, ThemeKind::Sepia, ThemeKind::Dusk, ThemeKind::Night];

    pub fn name(self) -> &'static str {
        match self {
            ThemeKind::Paper => "Paper",
            ThemeKind::Sepia => "Sepia",
            ThemeKind::Dusk => "Dusk",
            ThemeKind::Night => "Night",
        }
    }

    pub fn palette(self) -> Palette {
        let hex = |s: &str| Color32::from_hex(s).expect("valid colour");
        match self {
            ThemeKind::Paper => Palette {
                bg: hex("#FBF8F2"),
                text: hex("#22201C"),
                muted: hex("#8C8579"),
                faint: hex("#E6E0D4"),
                surface: hex("#FFFDF9"),
                accent: hex("#A65F3B"),
                dark: false,
            },
            ThemeKind::Sepia => Palette {
                bg: hex("#F3EAD7"),
                text: hex("#3A2F23"),
                muted: hex("#8A7A63"),
                faint: hex("#E0D3B8"),
                surface: hex("#F8F1E2"),
                accent: hex("#9C5A34"),
                dark: false,
            },
            ThemeKind::Dusk => Palette {
                bg: hex("#2A2C30"),
                text: hex("#D9D5CD"),
                muted: hex("#8E8B85"),
                faint: hex("#3B3E44"),
                surface: hex("#33363B"),
                accent: hex("#D9A066"),
                dark: true,
            },
            ThemeKind::Night => Palette {
                bg: hex("#0F0F10"),
                text: hex("#B8B2A7"),
                muted: hex("#6E6A64"),
                faint: hex("#252527"),
                surface: hex("#1A1A1C"),
                accent: hex("#C99A5B"),
                dark: true,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub surface: Color32,
    pub accent: Color32,
    pub dark: bool,
}

impl Palette {
    /// Make egui's own widgets (scroll bars, selection…) sit quietly within the theme.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        visuals.panel_fill = self.bg;
        visuals.window_fill = self.surface;
        visuals.extreme_bg_color = self.bg;
        visuals.faint_bg_color = self.faint;
        visuals.override_text_color = Some(self.text);
        visuals.hyperlink_color = self.accent;
        visuals.selection.bg_fill = self.accent.gamma_multiply(0.35);
        visuals.selection.stroke.color = self.accent;
        visuals.window_stroke.color = self.faint;
        visuals.widgets.noninteractive.bg_stroke.color = self.faint;
        visuals.widgets.inactive.bg_fill = self.faint;
        visuals.widgets.inactive.weak_bg_fill = self.faint;
        visuals.widgets.hovered.bg_fill = self.muted.gamma_multiply(0.5);
        visuals.widgets.active.bg_fill = self.muted;
        ctx.set_visuals(visuals);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Typeface {
    Literata,
    Garamond,
    Inter,
}

impl Typeface {
    pub const ALL: [Typeface; 3] = [Typeface::Literata, Typeface::Garamond, Typeface::Inter];

    pub fn name(self) -> &'static str {
        match self {
            Typeface::Literata => "Literata",
            Typeface::Garamond => "Garamond",
            Typeface::Inter => "Inter",
        }
    }

    pub fn family(self, italic: bool) -> FontFamily {
        let base = match self {
            Typeface::Literata => "literata",
            Typeface::Garamond => "garamond",
            Typeface::Inter => "inter",
        };
        FontFamily::Name(if italic { format!("{base}-italic") } else { base.to_string() }.into())
    }

    /// Optical compensation so that every face reads at a similar size.
    pub fn size_scale(self) -> f32 {
        match self {
            Typeface::Literata => 1.0,
            Typeface::Garamond => 1.12,
            Typeface::Inter => 0.95,
        }
    }

    /// Whether the font has an optical-size axis we can drive from the point size.
    pub fn opsz_range(self) -> Option<(f32, f32)> {
        match self {
            Typeface::Literata => Some((7.0, 72.0)),
            Typeface::Inter => Some((14.0, 32.0)),
            Typeface::Garamond => None,
        }
    }

    pub fn font(self, size: f32, italic: bool) -> FontId {
        FontId::new(size, self.family(italic))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spacing {
    Compact,
    Normal,
    Airy,
}

impl Spacing {
    pub const ALL: [Spacing; 3] = [Spacing::Compact, Spacing::Normal, Spacing::Airy];

    pub fn factor(self) -> f32 {
        match self {
            Spacing::Compact => 1.32,
            Spacing::Normal => 1.5,
            Spacing::Airy => 1.72,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Spacing::Compact => "Snug",
            Spacing::Normal => "Normal",
            Spacing::Airy => "Airy",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Measure {
    Narrow,
    Medium,
    Wide,
}

impl Measure {
    pub const ALL: [Measure; 3] = [Measure::Narrow, Measure::Medium, Measure::Wide];

    /// Line length in ems.
    pub fn ems(self) -> f32 {
        match self {
            Measure::Narrow => 27.0,
            Measure::Medium => 33.0,
            Measure::Wide => 41.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Measure::Narrow => "Narrow",
            Measure::Medium => "Medium",
            Measure::Wide => "Wide",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeKind,
    pub typeface: Typeface,
    pub size: f32,
    pub spacing: Spacing,
    pub measure: Measure,
    pub justify: bool,
    pub hyphenate: bool,
    pub indent: bool,
    pub show_progress: bool,
    pub animate: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeKind::Paper,
            typeface: Typeface::Literata,
            size: 19.0,
            spacing: Spacing::Normal,
            measure: Measure::Medium,
            justify: true,
            hyphenate: true,
            indent: true,
            show_progress: true,
            animate: true,
        }
    }
}

impl Settings {
    pub const MIN_SIZE: f32 = 13.0;
    pub const MAX_SIZE: f32 = 34.0;
}
