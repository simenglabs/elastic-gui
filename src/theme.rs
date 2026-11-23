//! Shared desktop design: cool surfaces, accessible text, and vector navigation.
use eframe::egui::{self, Color32, FontId, RichText, Stroke, Vec2, pos2, vec2};

pub const BLUE: Color32 = Color32::from_rgb(40, 120, 216);
pub const GREEN: Color32 = Color32::from_rgb(35, 139, 88);
pub const AMBER: Color32 = Color32::from_rgb(157, 105, 12);
pub const RED: Color32 = Color32::from_rgb(194, 65, 83);
pub const ROW_HEIGHT: f32 = 36.0;

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color32,
    pub surface: Color32,
    pub subtle: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub selected: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            background: Color32::from_rgb(18, 27, 42),
            surface: Color32::from_rgb(25, 38, 56),
            subtle: Color32::from_rgb(31, 46, 65),
            border: Color32::from_rgb(52, 70, 92),
            text: Color32::from_rgb(228, 237, 247),
            muted: Color32::from_rgb(161, 181, 203),
            selected: Color32::from_rgb(35, 65, 102),
        }
    } else {
        Palette {
            background: Color32::from_rgb(239, 245, 249),
            surface: Color32::WHITE,
            subtle: Color32::from_rgb(246, 249, 252),
            border: Color32::from_rgb(223, 232, 240),
            text: Color32::from_rgb(22, 35, 51),
            muted: Color32::from_rgb(92, 110, 132),
            selected: Color32::from_rgb(229, 240, 254),
        }
    }
}

pub fn apply(ctx: &egui::Context, dark: bool) {
    // Pin the chosen theme before selecting its style. Otherwise the first
    // native system-theme event can switch back to an uncustomized style.
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    let p = palette(dark);
    let mut style = (*ctx.global_style()).clone();
    style.text_styles = [
        (egui::TextStyle::Heading, FontId::proportional(24.0)),
        (egui::TextStyle::Body, FontId::proportional(14.0)),
        (egui::TextStyle::Button, FontId::proportional(14.0)),
        (egui::TextStyle::Small, FontId::proportional(12.0)),
        (egui::TextStyle::Monospace, FontId::monospace(14.0)),
    ]
    .into();
    style.spacing.item_spacing = vec2(10.0, 10.0);
    style.spacing.button_padding = vec2(12.0, 7.0);
    style.spacing.interact_size = vec2(36.0, 34.0);
    style.spacing.combo_width = 160.0;
    style.spacing.text_edit_width = 280.0;
    style.visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let v = &mut style.visuals;
    v.override_text_color = Some(p.text);
    v.weak_text_color = Some(p.muted);
    v.panel_fill = p.background;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.surface);
    v.faint_bg_color = p.subtle;
    v.code_bg_color = p.subtle;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = 14.into();
    v.menu_corner_radius = 10.into();
    v.hyperlink_color = if dark {
        Color32::from_rgb(132, 187, 255)
    } else {
        BLUE
    };
    v.selection.bg_fill = p.selected;
    v.selection.stroke = Stroke::new(1.0, v.hyperlink_color);
    v.warn_fg_color = if dark {
        Color32::from_rgb(239, 190, 93)
    } else {
        AMBER
    };
    v.error_fg_color = if dark {
        Color32::from_rgb(255, 142, 157)
    } else {
        RED
    };
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.corner_radius = 7.into();
        widget.bg_stroke = Stroke::new(1.0, p.border);
        widget.fg_stroke = Stroke::new(1.2, p.text);
        widget.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_fill = p.surface;
    v.widgets.noninteractive.weak_bg_fill = p.surface;
    v.widgets.inactive.bg_fill = p.subtle;
    v.widgets.inactive.weak_bg_fill = p.subtle;
    v.widgets.hovered.bg_fill = p.selected;
    v.widgets.hovered.weak_bg_fill = p.selected;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, v.hyperlink_color);
    v.widgets.active.bg_fill = p.selected;
    v.widgets.active.weak_bg_fill = p.selected;
    v.widgets.active.bg_stroke = Stroke::new(1.0, BLUE);
    v.widgets.open.bg_fill = p.selected;
    v.widgets.open.weak_bg_fill = p.selected;
    ctx.set_global_style(style);
}

pub fn card(ui: &egui::Ui) -> egui::Frame {
    let p = palette(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(14)
        .inner_margin(16)
}

pub fn workspace(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(palette(ui.visuals().dark_mode).background)
        .inner_margin(20)
}

pub fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
        .fill(BLUE)
        .stroke(Stroke::NONE)
}

pub fn dot(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(10.0, 16.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.5, color);
    response
}

pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.12))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(12.0).strong());
        });
}

pub fn header_gradient(ui: &egui::Ui, rect: egui::Rect) {
    let (left, right) = if ui.visuals().dark_mode {
        (
            Color32::from_rgb(30, 54, 80),
            Color32::from_rgb(35, 70, 105),
        )
    } else {
        (
            Color32::from_rgb(213, 234, 247),
            Color32::from_rgb(158, 197, 237),
        )
    };
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), left);
    mesh.colored_vertex(rect.right_top(), right);
    mesh.colored_vertex(rect.right_bottom(), right);
    mesh.colored_vertex(rect.left_bottom(), left);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
}

/// Vector icons stay crisp at any display scale and need no symbol font.
#[derive(Clone, Copy)]
pub enum Icon {
    Search,
    Code,
    Dashboard,
    Database,
    Cluster,
}

fn icon(ui: &egui::Ui, center: egui::Pos2, kind: Icon, color: Color32) {
    let p = ui.painter();
    let s = Stroke::new(1.6, color);
    let line = |a: Vec2, b: Vec2| {
        p.line_segment([center + a, center + b], s);
    };
    match kind {
        Icon::Search => {
            p.circle_stroke(center - vec2(2.0, 2.0), 5.5, s);
            line(vec2(2.0, 2.0), vec2(7.0, 7.0));
        }
        Icon::Code => {
            for sign in [-1.0, 1.0] {
                line(vec2(sign * 4.0, -5.0), vec2(sign * 8.0, 0.0));
                line(vec2(sign * 8.0, 0.0), vec2(sign * 4.0, 5.0));
            }
            line(vec2(2.0, -7.0), vec2(-2.0, 7.0));
        }
        Icon::Dashboard => {
            for x in [-4.5, 4.5] {
                for y in [-4.5, 4.5] {
                    p.rect_stroke(
                        egui::Rect::from_center_size(center + vec2(x, y), vec2(6.0, 6.0)),
                        1.5,
                        s,
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        Icon::Database => {
            for y in [-6.0, 0.0, 6.0] {
                p.rect_stroke(
                    egui::Rect::from_center_size(center + vec2(0.0, y), vec2(15.0, 5.0)),
                    2.0,
                    s,
                    egui::StrokeKind::Inside,
                );
            }
        }
        Icon::Cluster => {
            for delta in [vec2(0.0, -6.0), vec2(-7.0, 6.0), vec2(7.0, 6.0)] {
                line(Vec2::ZERO, delta);
                p.circle_filled(center + delta, 3.0, color);
            }
        }
    }
}

pub fn nav_button(
    ui: &mut egui::Ui,
    label: &str,
    kind: Icon,
    selected: bool,
    compact: bool,
) -> egui::Response {
    let p = palette(ui.visuals().dark_mode);
    let width = ui.available_width();
    let response = ui.add(
        egui::Button::new("")
            .min_size(vec2(width, 44.0))
            .selected(selected)
            .frame_when_inactive(selected)
            .corner_radius(10),
    );
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, selected, label));
    let color = if selected {
        ui.visuals().hyperlink_color
    } else {
        p.muted
    };
    let center = pos2(response.rect.left() + 22.0, response.rect.center().y);
    icon(ui, center, kind, color);
    if !compact {
        ui.painter().text(
            pos2(response.rect.left() + 44.0, center.y),
            egui::Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.0),
            if selected { color } else { p.text },
        );
    }
    response.on_hover_text(label)
}

pub fn empty_state(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.25).min(120.0));
        ui.label(RichText::new(title).size(21.0).strong());
        ui.add_space(4.0);
        ui.label(RichText::new(description).color(palette(ui.visuals().dark_mode).muted));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chosen_theme_survives_native_system_theme_events() {
        let ctx = egui::Context::default();
        for dark in [false, true] {
            apply(&ctx, dark);
            let input = egui::RawInput {
                system_theme: Some(if dark {
                    egui::Theme::Light
                } else {
                    egui::Theme::Dark
                }),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                assert_eq!(ui.visuals().dark_mode, dark);
                assert_eq!(ui.visuals().panel_fill, palette(dark).background);
                let button = ui.button("Theme control");
                assert!(button.rect.height() >= 34.0);
            });
            output.textures_delta.clear();
        }
    }
}
