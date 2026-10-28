//! The NyForge mark: a USB stick on a blue tile with a spark, drawn from the PNGs that `tools/make-logo.py` writes
//! (the same artwork as the app icon, the README and the installers). Three sizes are carried so a small mark is
//! never a shrunken big one.

use eframe::egui::{self, pos2, Color32, Id, Painter, Rect, TextureHandle, TextureOptions};

const SIZES: [(u32, &[u8]); 3] = [
    (64, include_bytes!("../../../assets/logo/nyforge-64.png")),
    (128, include_bytes!("../../../assets/logo/nyforge-128.png")),
    (256, include_bytes!("../../../assets/logo/nyforge-256.png")),
];

fn texture(ctx: &egui::Context, size: u32, png: &[u8]) -> Option<TextureHandle> {
    let id = Id::new(("nyforge-logo", size));
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return Some(t);
    }
    let icon = eframe::icon_data::from_png_bytes(png).ok()?;
    let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
    let t = ctx.load_texture(format!("nyforge-logo-{size}"), image, TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}

/// Draws the logo into `rect` (it is square).
pub fn paint(p: &Painter, rect: Rect) {
    let ctx = p.ctx();
    let want = rect.width() * ctx.pixels_per_point();
    let (size, png) = SIZES.iter().copied().find(|(s, _)| *s as f32 >= want).unwrap_or(SIZES[2]);
    if let Some(t) = texture(ctx, size, png) {
        p.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
}
