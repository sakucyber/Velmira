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
            bg: Color32::from_rgb(20, 22, 26),
            surface: Color32::from_rgb(29, 32, 38),
            surface2: Color32::from_rgb(37, 41, 49),
            ink: Color32::from_rgb(236, 234, 228),
            muted: Color32::from_rgb(167, 171, 179),
            line: Color32::from_rgb(48, 53, 64),
            accent: Color32::from_rgb(90, 174, 147),
            on_accent: Color32::from_rgb(18, 31, 27),
            accent2: Color32::from_rgb(116, 196, 171),
            accent_weak: Color32::from_rgb(31, 55, 47),
            danger: Color32::from_rgb(235, 149, 142),
            success: Color32::from_rgb(116, 196, 171),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(247, 244, 238),
            surface: Color32::from_rgb(255, 253, 249),
            surface2: Color32::from_rgb(241, 238, 231),
            ink: Color32::from_rgb(28, 27, 25),
            muted: Color32::from_rgb(119, 115, 107),
            line: Color32::from_rgb(227, 222, 212),
            accent: Color32::from_rgb(46, 125, 107),
            on_accent: Color32::from_rgb(255, 253, 249),
            accent2: Color32::from_rgb(79, 155, 135),
            accent_weak: Color32::from_rgb(228, 240, 235),
            danger: Color32::from_rgb(179, 64, 47),
            success: Color32::from_rgb(46, 125, 107),
        }
    }
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub space_xs: f32,
    pub space_sm: f32,
    pub space_md: f32,
    pub space_lg: f32,
    pub space_xl: f32,
    pub radius_sm: egui::CornerRadius,
    pub radius_md: egui::CornerRadius,
    pub radius_lg: egui::CornerRadius,
    pub control_height: f32,
    pub touch_min: f32,
    pub motion_fast: f64,
    pub motion_panel: f64,
    pub shadow_alpha: u8,
    pub card_shadow_alpha: u8,
}

impl Tokens {
    pub fn desktop() -> Self {
        Self {
            space_xs: 4.0,
            space_sm: 8.0,
            space_md: 12.0,
            space_lg: 18.0,
            space_xl: 26.0,
            radius_sm: 8.into(),
            radius_md: 12.into(),
            radius_lg: 18.into(),
            control_height: 34.0,
            touch_min: 34.0,
            motion_fast: 0.14,
            motion_panel: 0.22,
            shadow_alpha: 92,
            card_shadow_alpha: 22,
        }
    }

    #[cfg(any(target_os = "android", test))]
    pub fn android(points_per_dp: f32) -> Self {
        let points_per_dp = if points_per_dp.is_finite() && points_per_dp > 0.0 {
            points_per_dp
        } else {
            1.0
        };
        let mut tokens = Self::desktop();
        tokens.space_xs = 4.0 * points_per_dp;
        tokens.space_sm = 8.0 * points_per_dp;
        tokens.space_md = 12.0 * points_per_dp;
        tokens.space_lg = 18.0 * points_per_dp;
        tokens.space_xl = 26.0 * points_per_dp;
        tokens.control_height = dp_to_points(44.0, points_per_dp);
        tokens.touch_min = dp_to_points(44.0, points_per_dp);
        tokens.motion_fast = 0.20;
        tokens.motion_panel = 0.26;
        tokens.shadow_alpha = 58;
        tokens.card_shadow_alpha = 10;
        tokens
    }
}

#[cfg(any(target_os = "android", test))]
pub fn dp_to_points(dp: f32, points_per_dp: f32) -> f32 {
    dp * points_per_dp.max(0.0)
}

pub fn visuals_with_tokens(night: bool, tokens: Tokens) -> Visuals {
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
    v.window_corner_radius = tokens.radius_lg;
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(tokens.shadow_alpha),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if night { 92 } else { 32 }),
    };
    v.menu_corner_radius = tokens.radius_sm;
    v.selection.bg_fill = p.accent_weak;
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    v.text_cursor.stroke = Stroke::new(1.6_f32, p.accent);
    v.striped = false;
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;

    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.muted);
    v.widgets.noninteractive.corner_radius = tokens.radius_sm;
    v.widgets.noninteractive.expansion = 0.0;

    v.widgets.inactive.bg_fill = p.surface;
    v.widgets.inactive.weak_bg_fill = p.surface2;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.ink);
    v.widgets.inactive.corner_radius = tokens.radius_sm;
    v.widgets.inactive.expansion = 1.0;

    v.widgets.hovered.bg_fill = p.surface2;
    v.widgets.hovered.weak_bg_fill = p.surface2;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, mix(p.accent, p.line, 0.55));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, p.ink);
    v.widgets.hovered.corner_radius = tokens.radius_sm;
    v.widgets.hovered.expansion = 1.5;

    v.widgets.active.bg_fill = p.accent_weak;
    v.widgets.active.weak_bg_fill = p.accent_weak;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, p.accent);
    v.widgets.active.corner_radius = tokens.radius_sm;
    v.widgets.active.expansion = 1.0;

    v.widgets.open.bg_fill = p.surface2;
    v.widgets.open.weak_bg_fill = p.surface2;
    v.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.line);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, p.ink);
    v.widgets.open.corner_radius = tokens.radius_sm;
    v.widgets.open.expansion = 1.0;

    v
}

#[cfg(not(target_os = "android"))]
pub fn apply(ctx: &egui::Context, night: bool) {
    apply_with_tokens(ctx, night, Tokens::desktop());
}

pub fn apply_with_tokens(ctx: &egui::Context, night: bool, tokens: Tokens) {
    ctx.set_theme(if night {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    let mut style = (*ctx.style()).clone();
    style.visuals = visuals_with_tokens(night, tokens);
    style.animation_time = tokens.motion_fast as f32;
    style.scroll_animation = egui::style::ScrollAnimation {
        points_per_second: 900.0,
        duration: egui::Rangef::new(
            (tokens.motion_panel * 0.35) as f32,
            tokens.motion_panel as f32,
        ),
    };
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(tokens.touch_min, tokens.control_height);
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
