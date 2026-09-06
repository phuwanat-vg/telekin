//! The viewer's look: white and blue, and sized to be read at arm's length.
//!
//! Three ideas drive every choice here.
//!
//! **White ground, one blue.** Paper with a single accent, so the accent
//! always means something: it marks what is live, what is selected, and the
//! one action a screen is about. Nothing else competes for it.
//!
//! **Big enough to read across a bench.** This is used standing next to a
//! powered robot, often at arm's length rather than at a desk, so type and
//! controls are a size up from what a settings dialog would use. That is a
//! legibility decision, not a stylistic one.
//!
//! **Machine values are monospace.** Addresses, milliseconds, frame rates —
//! fixed-width digits do not jitter as a number changes twenty times a
//! second, and the shape tells you at a glance that a value came from the
//! robot rather than from a person.

use egui::{Color32, CornerRadius, Stroke};

// One ramp from paper to ink, tinted blue throughout so white never reads as
// grey. Named by role, so a call site never has to know which shade it is
// asking for.
/// The window itself.
pub const BASE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// Panels and strips: a hair off white, separated by tone rather than a line.
pub const SURFACE: Color32 = Color32::from_rgb(0xF4, 0xF7, 0xFB);
/// Input interiors and resting controls.
const RAISED: Color32 = Color32::from_rgb(0xEC, 0xF2, 0xF9);
/// Under the pointer.
const HOVER: Color32 = Color32::from_rgb(0xDD, 0xE9, 0xF7);
/// Held down, and the selected row.
const ACTIVE: Color32 = Color32::from_rgb(0xCB, 0xDE, 0xF6);
/// Hairlines. A divider should be felt more than seen.
const BORDER: Color32 = Color32::from_rgb(0xC2, 0xD0, 0xE1);

/// The blue. Dark enough to carry white text on a button and to be read as a
/// label on paper — the pale cyan that worked on near-black does neither.
pub const ACCENT: Color32 = Color32::from_rgb(0x15, 0x63, 0xC2);
/// Hover and focus. On paper the accent deepens rather than brightens.
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(0x0D, 0x47, 0x91);

/// Body text. Essentially black — the blue cast is there so it sits in the
/// same family as the rest, not to soften it. Against the white ground this
/// is roughly 18:1, which is what makes it readable across a workbench.
pub const TEXT: Color32 = Color32::from_rgb(0x0A, 0x0E, 0x14);
/// Labels, units, hints. Secondary by being lighter than the body text, not
/// by being close to the background: at the previous value it was the first
/// thing to disappear under a window's glare.
pub const MUTED: Color32 = Color32::from_rgb(0x3D, 0x4A, 0x5A);
/// Live and healthy. Darkened from the dark theme's mint, which vanished on
/// white.
pub const GOOD: Color32 = Color32::from_rgb(0x0E, 0x7A, 0x4B);

// Black, used where the application's own chrome meets the robot's content.
// A page of pale blue-greys has nothing to push against; one black band gives
// the eye a fixed edge and says plainly which part of the window is Telekin
// and which part is the machine at the other end.
/// The session bar across the top.
pub const BAR: Color32 = Color32::from_rgb(0x0C, 0x0F, 0x14);
/// Text on the bar.
pub const BAR_TEXT: Color32 = Color32::from_rgb(0xF2, 0xF5, 0xF9);
/// Labels and units on the bar.
pub const BAR_MUTED: Color32 = Color32::from_rgb(0x93, 0xA2, 0xB5);
/// The accent as it must appear on black: the paper blue is too dark there.
pub const BAR_ACCENT: Color32 = Color32::from_rgb(0x5E, 0xAD, 0xF5);

/// The outline around a clickable row.
///
/// Stronger than a hairline divider on purpose: it is not separating two
/// things, it is drawing the edge of a target.
pub const ROW_EDGE: Color32 = Color32::from_rgb(0xB6, 0xC6, 0xDA);

/// Behind the remote screen, and only there.
///
/// The one place that stays dark. A bright surround around a video frame is
/// glare, it shifts the apparent colour of the robot's own desktop, and the
/// letterbox bars would otherwise be the brightest thing in the window.
pub const VIDEO_BACKDROP: Color32 = Color32::from_rgb(0x0B, 0x10, 0x18);

/// Minimum height for anything clickable.
///
/// egui's default is around 20 px, which is a fiddly target on a trackpad and
/// an impossible one for someone standing beside a robot.
pub const CONTROL_HEIGHT: f32 = 42.0;

pub fn apply(ctx: &egui::Context) {
    // egui keeps a style per theme and follows the OS. This tool has one look
    // by design, so pin the theme and then style it rather than letting a
    // robot bay's machine and an office machine disagree about what it is.
    ctx.set_theme(egui::Theme::Light);
    ctx.all_styles_mut(configure);
}

fn configure(style: &mut egui::Style) {
    use egui::{FontFamily::Monospace, FontFamily::Proportional, FontId, TextStyle};
    // A size up from a desktop application's defaults, on purpose — see the
    // module docs. Nothing here is decorative; it is all read.
    style.text_styles = [
        (TextStyle::Heading, FontId::new(34.0, Proportional)),
        (TextStyle::Body, FontId::new(17.0, Proportional)),
        (TextStyle::Button, FontId::new(17.0, Proportional)),
        (TextStyle::Small, FontId::new(14.0, Proportional)),
        (TextStyle::Monospace, FontId::new(16.0, Monospace)),
    ]
    .into();

    // Generous spacing is what replaces the borders that were removed.
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(10.0, 12.0);
    s.button_padding = egui::vec2(18.0, 11.0);
    s.interact_size = egui::vec2(48.0, CONTROL_HEIGHT);
    s.slider_width = 220.0;
    s.slider_rail_height = 4.0; // a thin rail reads as a scale, not a trough
    s.combo_width = 240.0;
    s.text_edit_width = 340.0;
    s.window_margin = egui::Margin::same(16);
    s.menu_margin = egui::Margin::same(10);
    s.indent = 18.0;

    let mut v = egui::Visuals::light();
    v.panel_fill = SURFACE;
    v.window_fill = BASE;
    v.extreme_bg_color = BASE;
    v.faint_bg_color = RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.override_text_color = Some(TEXT);
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = Color32::from_rgb(0x9A, 0x5D, 0x00);
    v.error_fg_color = Color32::from_rgb(0xB4, 0x22, 0x22);
    v.selection.bg_fill = ACTIVE;
    v.selection.stroke = Stroke::new(1.0, ACCENT);

    // A small radius throughout: enough to look considered, not so much that
    // controls turn into pills.
    let radius = CornerRadius::same(6);
    let w = &mut v.widgets;

    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = radius;

    // Resting: filled *and* outlined. On near-black a fill alone read as a
    // control; on paper it does not, because the fill is only a few percent
    // away from the ground behind it.
    w.inactive.bg_fill = RAISED;
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = radius;

    // Hover: the accent appears only here and on the primary action, which is
    // what keeps one colour meaningful.
    w.hovered.bg_fill = HOVER;
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0; // no jump on hover; the outline is the signal

    w.active.bg_fill = ACTIVE;
    w.active.weak_bg_fill = ACTIVE;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT_BRIGHT);
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = HOVER;
    w.open.weak_bg_fill = HOVER;
    w.open.bg_stroke = Stroke::new(1.0, ACCENT);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    w.open.corner_radius = radius;

    style.visuals = v;
}

// The mark, as fractions of its square so the drawn version and the rasterised
// window icon cannot drift apart.
/// Radius of the ring: the robot's body.
const MARK_RING_R: f32 = 0.36;
/// Stroke width of the ring.
const MARK_STROKE: f32 = 0.09;
/// The dot inside it: the operator, now in there.
const MARK_DOT_R: f32 = 0.11;
/// Width of the break in the ring, in degrees, centred on its left side.
const MARK_GAP_DEG: f32 = 62.0;

/// The mark: a broken ring with a dot inside it.
///
/// The ring is the robot's body and the dot is whoever is now in there. The
/// ring does not close — nothing physical crosses the distance, which is the
/// whole idea of the name.
///
/// An earlier version drew the gap as a separate line reaching in from
/// outside. It did not survive being small: at 30 px there is no room for both
/// a ring and anything beside it. Putting the gap *in* the ring says the same
/// thing using space the mark already owns.
///
/// Takes its colour, because the mark appears both on paper and on the black
/// bar, and the blue that carries on white disappears on black.
///
/// Drawn rather than shipped as an image so it stays sharp at any scale and
/// adds no asset to carry around.
pub fn logo(ui: &mut egui::Ui, size: f32, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let c = rect.center();
    let stroke = Stroke::new(size * MARK_STROKE, colour);
    let r = size * MARK_RING_R;

    // Enough segments that the curve stays smooth at the sizes this is used
    // at, and few enough to stay cheap on a frame that redraws constantly.
    const STEPS: usize = 48;
    let half_gap = MARK_GAP_DEG.to_radians() * 0.5;
    let start = std::f32::consts::PI + half_gap;
    let sweep = std::f32::consts::TAU - MARK_GAP_DEG.to_radians();
    let arc: Vec<egui::Pos2> = (0..=STEPS)
        .map(|i| {
            let a = start + sweep * (i as f32) / (STEPS as f32);
            egui::pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();

    let painter = ui.painter();
    painter.add(egui::Shape::line(arc, stroke));
    painter.circle_filled(c, size * MARK_DOT_R, colour);
}

/// The same mark as pixels, for the window and taskbar icon.
///
/// Rasterised here rather than shipped as a .ico so there is exactly one
/// definition of the shape: change a constant above and both follow.
/// Supersampled 3x3, which is enough to keep a small ring from looking ragged.
pub fn icon_rgba(px: u32) -> Vec<u8> {
    const SS: i32 = 3;
    let mut out = Vec::with_capacity((px * px * 4) as usize);
    let [r, g, b, _] = ACCENT.to_array();
    let half_gap = MARK_GAP_DEG.to_radians() * 0.5;

    for y in 0..px {
        for x in 0..px {
            let mut hits = 0;
            for sy in 0..SS {
                for sx in 0..SS {
                    // Sample centres, in a square running -0.5..0.5.
                    let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / px as f32 - 0.5;
                    let v = (y as f32 + (sy as f32 + 0.5) / SS as f32) / px as f32 - 0.5;
                    let d = u.hypot(v);

                    // The break sits on the left, so a sample is in it when its
                    // angle is within half the gap of pointing that way.
                    let in_gap = v.atan2(u).abs() > std::f32::consts::PI - half_gap;
                    let on_ring = (d - MARK_RING_R).abs() <= MARK_STROKE * 0.5 && !in_gap;
                    if on_ring || d <= MARK_DOT_R {
                        hits += 1;
                    }
                }
            }
            out.extend_from_slice(&[r, g, b, (255 * hits / (SS * SS)) as u8]);
        }
    }
    out
}

/// A section heading: small, tracked capitals over a hairline.
///
/// This is what replaced the bordered card. A label and a rule group a set of
/// controls just as clearly, without drawing a box around everything.
pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(track(title))
                .size(13.0)
                .color(TEXT)
                .strong(),
        );
    });
    ui.add_space(2.0);
    let rect = ui.available_rect_before_wrap();
    let y = rect.top();
    ui.painter().line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        Stroke::new(1.0, BORDER),
    );
    ui.add_space(12.0);
}

/// Letter-spacing, which egui does not offer, done the only way available:
/// by inserting thin spaces. Worth it on the few short capitalised labels
/// where the tracking is what makes them read as headings rather than text.
fn track(text: &str) -> String {
    text.to_uppercase()
        .chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join("\u{2009}")
}

/// The one action a screen is about.
pub fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_owned())
            .size(18.0)
            .color(Color32::WHITE)
            .strong(),
    )
    .fill(ACCENT)
    .corner_radius(CornerRadius::same(6))
}

/// A field label above its input.
pub fn field_label(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(15.0).color(MUTED));
}

/// A machine-read value: address, duration, rate. Monospace so a changing
/// number does not shift the text beside it.
pub fn value(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).monospace().color(TEXT)
}

/// Background for a meter's track.
pub fn track_colour() -> Color32 {
    RAISED
}

// ---------------------------------------------------------------------------
// Dense lists
// ---------------------------------------------------------------------------

/// Tighten this `Ui` for a file list.
///
/// The file panes used to carry a whole second palette, because the app around
/// them was near-black and a list of a hundred filenames wanted paper. Now
/// that everything is paper, only the *spacing* still needs to differ: a list
/// is dense text to be scanned, not a form to be filled in, and the row height
/// sized for a thumb beside a robot showed six files on a laptop screen.
pub fn dense_list(ui: &mut egui::Ui) {
    let s = &mut ui.style_mut().spacing;
    s.item_spacing = egui::vec2(8.0, 3.0);
    s.button_padding = egui::vec2(10.0, 6.0);
    // Only the rows shrink: the path bar and the folder buttons ask for
    // `CONTROL_HEIGHT` explicitly and keep it.
    s.interact_size = egui::vec2(40.0, 22.0);
}

/// The black session bar, with its controls restyled to sit on it.
///
/// Buttons here keep the same shape as everywhere else; only their tones are
/// inverted, so the bar reads as the same application rather than as a
/// separate widget set bolted on top.
pub fn bar(ui: &mut egui::Ui) {
    let v = &mut ui.style_mut().visuals;
    v.override_text_color = Some(BAR_TEXT);
    let w = &mut v.widgets;
    let edge = Color32::from_rgb(0x2A, 0x33, 0x40);

    w.noninteractive.fg_stroke = Stroke::new(1.0, BAR_TEXT);
    w.noninteractive.bg_stroke = Stroke::new(1.0, edge);

    w.inactive.bg_fill = Color32::from_rgb(0x18, 0x1D, 0x25);
    w.inactive.weak_bg_fill = Color32::from_rgb(0x18, 0x1D, 0x25);
    w.inactive.bg_stroke = Stroke::new(1.0, edge);
    w.inactive.fg_stroke = Stroke::new(1.0, BAR_TEXT);

    w.hovered.bg_fill = Color32::from_rgb(0x22, 0x2A, 0x36);
    w.hovered.weak_bg_fill = Color32::from_rgb(0x22, 0x2A, 0x36);
    w.hovered.bg_stroke = Stroke::new(1.0, BAR_ACCENT);
    w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);

    w.active.bg_fill = Color32::from_rgb(0x14, 0x33, 0x55);
    w.active.weak_bg_fill = Color32::from_rgb(0x14, 0x33, 0x55);
    w.active.bg_stroke = Stroke::new(1.0, BAR_ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
}

/// A panel frame in one of the palette's tones.
pub fn panel(fill: Color32, margin: i8) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::same(margin))
}

// ---------------------------------------------------------------------------
// File and folder icons
// ---------------------------------------------------------------------------

/// A folder, drawn: body plus the raised tab along its top left.
///
/// Drawn rather than a font glyph so it is the same on Windows and Linux —
/// emoji and dingbat coverage differs between the two, and a missing glyph in
/// a file list is a row you cannot read.
pub fn folder_icon(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let w = size * 0.86;
    let h = size * 0.68;
    let x = rect.left() + (size - w) * 0.5;
    let y = rect.top() + (size - h) * 0.5;
    let r = CornerRadius::same((size * 0.12) as u8);

    let painter = ui.painter();
    // The tab, drawn first so the body's top edge covers where they meet.
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(x, y - size * 0.12),
            egui::vec2(w * 0.45, size * 0.24),
        ),
        CornerRadius::same((size * 0.08) as u8),
        FOLDER,
    );
    painter.rect_filled(
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)),
        r,
        FOLDER,
    );
}

/// A page with its corner turned down.
pub fn file_icon(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let w = size * 0.66;
    let h = size * 0.82;
    let x = rect.left() + (size - w) * 0.5;
    let y = rect.top() + (size - h) * 0.5;
    let fold = size * 0.24;

    // The page outline, with the top right corner cut off.
    let body = vec![
        egui::pos2(x, y),
        egui::pos2(x + w - fold, y),
        egui::pos2(x + w, y + fold),
        egui::pos2(x + w, y + h),
        egui::pos2(x, y + h),
    ];
    let painter = ui.painter();
    painter.add(egui::Shape::convex_polygon(
        body,
        PAGE,
        Stroke::new(1.0, PAGE_EDGE),
    ));
    // The turned corner, in the edge colour so it reads as a fold.
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(x + w - fold, y),
            egui::pos2(x + w, y + fold),
            egui::pos2(x + w - fold, y + fold),
        ],
        PAGE_EDGE,
        Stroke::NONE,
    ));
}

/// Manila, because every file manager since 1984 has used it and the shape is
/// recognised before the colour is.
const FOLDER: Color32 = Color32::from_rgb(0xE8, 0xB1, 0x35);
const PAGE: Color32 = Color32::from_rgb(0xFA, 0xFB, 0xFC);
const PAGE_EDGE: Color32 = Color32::from_rgb(0xA8, 0xB3, 0xC2);

/// Which way an arrow button points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Up,
    Left,
    Right,
}

/// A button with a drawn arrow on it.
///
/// The glyph version of this shipped as an empty box: egui's bundled fonts do
/// not cover `↑`, and a missing glyph on the one control that moves files is
/// not a cosmetic problem. Drawing the shape sidesteps font coverage entirely,
/// which also means it looks the same on Windows and on the robot.
pub fn arrow_button(ui: &mut egui::Ui, size: egui::Vec2, dir: Arrow, hint: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let enabled = ui.is_enabled();

    let visuals = if !enabled {
        ui.visuals().widgets.noninteractive
    } else if response.is_pointer_button_down_on() {
        ui.visuals().widgets.active
    } else if response.hovered() {
        ui.visuals().widgets.hovered
    } else {
        ui.visuals().widgets.inactive
    };

    let painter = ui.painter();
    painter.rect(
        rect,
        visuals.corner_radius,
        visuals.bg_fill,
        visuals.bg_stroke,
        egui::StrokeKind::Inside,
    );

    // A chevron rather than a filled triangle: it stays legible at button
    // size and matches the hairline weight of everything else here.
    let c = rect.center();
    let r = size.min_elem() * 0.22;
    let (tip, wing_a, wing_b, tail) = match dir {
        Arrow::Up => (
            egui::pos2(c.x, c.y - r),
            egui::pos2(c.x - r * 0.8, c.y),
            egui::pos2(c.x + r * 0.8, c.y),
            egui::pos2(c.x, c.y + r),
        ),
        Arrow::Left => (
            egui::pos2(c.x - r, c.y),
            egui::pos2(c.x, c.y - r * 0.8),
            egui::pos2(c.x, c.y + r * 0.8),
            egui::pos2(c.x + r, c.y),
        ),
        Arrow::Right => (
            egui::pos2(c.x + r, c.y),
            egui::pos2(c.x, c.y - r * 0.8),
            egui::pos2(c.x, c.y + r * 0.8),
            egui::pos2(c.x - r, c.y),
        ),
    };
    let ink = if enabled {
        visuals.fg_stroke.color
    } else {
        visuals.fg_stroke.color.gamma_multiply(0.4)
    };
    let stroke = Stroke::new((size.min_elem() * 0.07).max(1.5), ink);
    painter.line_segment([tail, tip], stroke);
    painter.line_segment([wing_a, tip], stroke);
    painter.line_segment([wing_b, tip], stroke);

    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if hint.is_empty() {
        response
    } else {
        response.on_hover_text(hint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_has_a_ring_a_dot_and_a_break() {
        let px = 64u32;
        let rgba = icon_rgba(px);
        assert_eq!(rgba.len() as u32, px * px * 4);

        let alpha = |x: u32, y: u32| rgba[((y * px + x) * 4 + 3) as usize];
        let c = px / 2;
        let ring = (px as f32 * MARK_RING_R).round() as u32;

        assert_eq!(alpha(c, c), 255, "no dot in the middle");
        assert_eq!(alpha(c + ring, c), 255, "no ring on the right");
        assert_eq!(alpha(c, c - ring), 255, "no ring on top");
        // The break, which is what makes it mean anything.
        assert_eq!(alpha(c - ring, c), 0, "the ring closed up; the gap is gone");
        // Between dot and ring there is nothing, or the two have merged.
        let between = (px as f32 * (MARK_DOT_R + MARK_RING_R) * 0.5) as u32;
        assert_eq!(alpha(c + between, c), 0, "dot and ring have run together");
    }

    #[test]
    #[ignore = "writes the mark as raw RGBA for eyeballing; run with --ignored"]
    fn dump() {
        // Into a temp directory: a diagnostic should not leave anything behind
        // in the source tree for someone else to wonder about.
        let px = 256u32;
        let path = std::env::temp_dir().join("telekin-mark.rgba");
        std::fs::write(&path, icon_rgba(px)).expect("write");
        println!("wrote {} at {px}x{px}", path.display());
    }
}
