use eframe::egui::{self, Color32, FontId, Stroke, TextStyle, Visuals};

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub line: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub accent2: Color32,
    pub accent_weak: Color32,
    pub danger: Color32,
    pub success: Color32,
}

pub fn palette(night: bool) -> Palette {
    if night {
        Palette {
            bg: Color32::from_rgb(18, 20, 25),
            surface: Color32::from_rgb(27, 30, 37),
            surface2: Color32::from_rgb(34, 38, 47),
            ink: Color32::from_rgb(232, 228, 220),
            muted: Color32::from_rgb(169, 176, 187),
            line: Color32::from_rgb(44, 49, 58),
            accent: Color32::from_rgb(232, 147, 92),
            on_accent: Color32::from_rgb(32, 24, 20),
            accent2: Color32::from_rgb(242, 178, 122),
            accent_weak: Color32::from_rgb(58, 42, 33),
            danger: Color32::from_rgb(240, 168, 156),
            success: Color32::from_rgb(159, 215, 184),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(244, 241, 234),
            surface: Color32::from_rgb(255, 253, 248),
            surface2: Color32::from_rgb(250, 246, 238),
            ink: Color32::from_rgb(39, 34, 28),
            muted: Color32::from_rgb(111, 103, 94),
            line: Color32::from_rgb(227, 221, 209),
            accent: Color32::from_rgb(168, 67, 31),
            on_accent: Color32::from_rgb(255, 253, 248),
            accent2: Color32::from_rgb(217, 123, 63),
            accent_weak: Color32::from_rgb(243, 226, 211),
            danger: Color32::from_rgb(179, 64, 47),
            success: Color32::from_rgb(63, 127, 82),
        }
    }
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub fn apply(ctx: &egui::Context, night: bool) {
    let p = palette(night);
    let mut v = if night {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.dark_mode = night;
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.surface2;
    v.code_bg_color = p.surface2;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.accent2;
    v.error_fg_color = p.danger;
    v.window_stroke = Stroke::new(1.0_f32, p.line);
    v.window_corner_radius = 12.into();
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(if night { 130 } else { 42 }),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if night { 110 } else { 36 }),
    };
    v.menu_corner_radius = 10.into();
    v.selection.bg_fill = p.accent_weak;
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.text_cursor.stroke = Stroke::new(1.6_f32, p.accent);
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;

    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.muted);
    v.widgets.noninteractive.corner_radius = 8.into();
    v.widgets.noninteractive.expansion = 0.0;

    v.widgets.inactive.bg_fill = p.surface;
    v.widgets.inactive.weak_bg_fill = p.surface2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.ink);
    v.widgets.inactive.corner_radius = 8.into();
    v.widgets.inactive.expansion = 1.0;

    v.widgets.hovered.bg_fill = p.surface2;
    v.widgets.hovered.weak_bg_fill = p.surface2;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, mix(p.accent, p.line, 0.55));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.ink);
    v.widgets.hovered.corner_radius = 8.into();
    v.widgets.hovered.expansion = 1.5;

    v.widgets.active.bg_fill = p.accent_weak;
    v.widgets.active.weak_bg_fill = p.accent_weak;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.accent);
    v.widgets.active.corner_radius = 8.into();
    v.widgets.active.expansion = 1.0;

    v.widgets.open.bg_fill = p.surface2;
    v.widgets.open.weak_bg_fill = p.surface2;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, p.ink);
    v.widgets.open.corner_radius = 8.into();
    v.widgets.open.expansion = 1.0;

    ctx.set_theme(if night {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    let mut style = (*ctx.style()).clone();
    style.visuals = v;
    style.scroll_animation = egui::style::ScrollAnimation {
        points_per_second: 900.0,
        duration: egui::Rangef::new(0.08, 0.25),
    };
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(40.0, 30.0);
    style.spacing.slider_width = 130.0;
    style.spacing.combo_width = 140.0;
    style.spacing.indent = 18.0;
    style.spacing.icon_spacing = 6.0;
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(22.0));
    ctx.set_style(style);
}
