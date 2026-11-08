//! Look and feel: palette, fonts, spacing, and a few styled building blocks.
//! Everything visual lives here so the screens only describe layout.

use std::sync::Arc;

use disk_scavenger::views::Safety;
use eframe::egui::{
    self, Button, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Frame,
    Margin, Response, RichText, Sense, Stroke, TextStyle, Theme, Ui, Vec2, Visuals,
};

/// Colors for one theme. Pick with `palette(ui)`.
pub struct Palette {
    pub accent: Color32,
    pub safe: Color32,
    pub review: Color32,
    pub risky: Color32,
    /// Card / side-panel background, a step off the window background
    pub surface: Color32,
    pub border: Color32,
    pub muted: Color32,
    pub track: Color32,
}

const DARK: Palette = Palette {
    accent: Color32::from_rgb(0x3B, 0xA7, 0xB7),
    safe: Color32::from_rgb(0x4C, 0xC3, 0x8A),
    review: Color32::from_rgb(0xE3, 0xA7, 0x3A),
    risky: Color32::from_rgb(0xEF, 0x6B, 0x66),
    surface: Color32::from_rgb(0x1E, 0x22, 0x28),
    border: Color32::from_rgb(0x2E, 0x34, 0x3C),
    muted: Color32::from_rgb(0x8B, 0x94, 0x9E),
    track: Color32::from_rgb(0x2A, 0x2F, 0x36),
};

const LIGHT: Palette = Palette {
    accent: Color32::from_rgb(0x1F, 0x7A, 0x87),
    safe: Color32::from_rgb(0x2E, 0x8B, 0x57),
    review: Color32::from_rgb(0xB7, 0x79, 0x1F),
    risky: Color32::from_rgb(0xC9, 0x44, 0x3F),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    border: Color32::from_rgb(0xDD, 0xE1, 0xE6),
    muted: Color32::from_rgb(0x66, 0x6F, 0x7A),
    track: Color32::from_rgb(0xE6, 0xE9, 0xED),
};

pub fn palette(ui: &Ui) -> &'static Palette {
    if ui.visuals().dark_mode {
        &DARK
    } else {
        &LIGHT
    }
}

impl Palette {
    pub fn safety(&self, s: Safety) -> Color32 {
        match s {
            Safety::Safe(_) => self.safe,
            Safety::Review(_) => self.review,
            Safety::Risky(_) => self.risky,
        }
    }
}

/// Heavier weight for headings and key numbers (falls back to the regular font).
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Install fonts, both themes' visuals, and spacing. Call once at startup.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, semibold())),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 5.0);
        style.spacing.interact_size.y = 28.0;
        // Bigger checkboxes: easier to see and to hit
        style.spacing.icon_width = 18.0;
        style.spacing.icon_width_inner = 10.0;
    });
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let semibold_chain = fonts.families[&FontFamily::Proportional].clone();
    fonts.families.insert(semibold(), semibold_chain);

    // Native system fonts make it look like a Windows app rather than a demo.
    // Missing files are fine: egui's bundled fonts stay as the fallback.
    #[cfg(target_os = "windows")]
    for (name, file, family) in [
        ("segoe", "segoeui.ttf", FontFamily::Proportional),
        ("segoe-sb", "seguisb.ttf", semibold()),
        ("consolas", "consola.ttf", FontFamily::Monospace),
    ] {
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        if let Ok(bytes) = std::fs::read(std::path::Path::new(&dir).join("Fonts").join(file)) {
            fonts
                .font_data
                .insert(name.into(), Arc::new(FontData::from_owned(bytes)));
            if let Some(chain) = fonts.families.get_mut(&family) {
                chain.insert(0, name.into());
            }
        }
    }
    fonts
}

fn visuals(dark: bool) -> Visuals {
    let p = if dark { &DARK } else { &LIGHT };
    let mut v = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    // egui's default text is mid-gray; real text colors make it far easier to read
    let (text, text_strong) = if dark {
        (
            Color32::from_rgb(0xD5, 0xD9, 0xDE),
            Color32::from_rgb(0xF2, 0xF4, 0xF6),
        )
    } else {
        (
            Color32::from_rgb(0x2B, 0x31, 0x37),
            Color32::from_rgb(0x11, 0x14, 0x18),
        )
    };
    let (panel, extreme, faint, button, button_hover) = if dark {
        (
            Color32::from_rgb(0x16, 0x19, 0x1D),
            Color32::from_rgb(0x10, 0x12, 0x15),
            Color32::from_rgb(0x1B, 0x1F, 0x24),
            Color32::from_rgb(0x28, 0x2D, 0x34),
            Color32::from_rgb(0x32, 0x38, 0x41),
        )
    } else {
        (
            Color32::from_rgb(0xF4, 0xF5, 0xF7),
            Color32::WHITE,
            Color32::from_rgb(0xF0, 0xF2, 0xF5),
            Color32::from_rgb(0xE8, 0xEB, 0xEF),
            Color32::from_rgb(0xDD, 0xE1, 0xE6),
        )
    };
    v.panel_fill = panel;
    v.window_fill = p.surface;
    v.extreme_bg_color = extreme;
    v.faint_bg_color = faint;
    v.window_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;

    let radius = CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, text);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, text_strong);
    v.widgets.active.fg_stroke = Stroke::new(2.0, text_strong);
    v.widgets.open.fg_stroke = Stroke::new(1.0, text_strong);
    v.weak_text_color = Some(p.muted);
    v.widgets.inactive.weak_bg_fill = button;
    v.widgets.inactive.bg_fill = button;
    // Outline so unticked checkboxes and buttons stay visible on striped rows
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.muted.gamma_multiply(0.5));
    v.widgets.hovered.weak_bg_fill = button_hover;
    v.widgets.hovered.bg_fill = button_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.6));
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v
}

/// A raised, rounded container (start screen card, banners).
pub fn card(ui: &Ui) -> Frame {
    let p = palette(ui);
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(24))
}

/// Small rounded pill, e.g. `[review]`.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(9))
        .inner_margin(Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().color(color));
        });
}

/// Tinted message box. Returns true if its close button was clicked.
pub fn banner(ui: &mut Ui, color: Color32, text: &str, closable: bool) -> bool {
    let mut closed = false;
    Frame::new()
        .fill(color.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if closable {
                    // Lay out right-to-left so the ✕ hugs the edge and text wraps before it
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        closed = ui
                            .add(Button::new(RichText::new("✕").small()).frame(false))
                            .on_hover_text("Dismiss")
                            .clicked();
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::TOP), |ui| {
                            ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
                        });
                    });
                } else {
                    ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
                }
            });
        });
    closed
}

fn filled_button(text: &str, fill: Color32, enabled: bool, ui: &mut Ui) -> Response {
    let label = RichText::new(text).strong().color(Color32::WHITE);
    ui.add_enabled(
        enabled,
        Button::new(label)
            .fill(fill)
            .corner_radius(CornerRadius::same(6))
            .min_size(Vec2::new(0.0, 32.0)),
    )
}

/// Destructive action (move to trash).
pub fn danger_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let fill = palette(ui).risky;
    filled_button(text, fill, enabled, ui)
}

/// Regular framed button at the same height as the filled ones.
pub fn secondary_button(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    ui.add_enabled(enabled, Button::new(text).min_size(Vec2::new(0.0, 32.0)))
}

/// Thin horizontal bar filled to `fraction` (0..=1) of the available width.
pub fn share_bar(ui: &mut Ui, fraction: f32, color: Color32) {
    let p = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 5.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(3), p.track);
    let mut filled = rect;
    filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    if filled.width() > 0.5 {
        painter.rect_filled(filled, CornerRadius::same(3), color);
    }
}
