//! Simple line icons drawn with the painter, so they stay crisp at every size and match the text colour.

use eframe::egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Flash,
    Boot,
    Format,
    Home,
    Refresh,
    Usb,
    File,
    Check,
    Warn,
    Error,
    Info,
    Plus,
    Trash,
    Folder,
    Back,
    Shield,
}

/// Paints `icon` centred in `rect` with line width scaled to the size.
pub fn paint(p: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let c = rect.center();
    let u = rect.width().min(rect.height()) / 24.0; // the icons are designed on a 24-unit grid
    let w = (1.6 * u).max(1.0);
    let st = Stroke::new(w, color);
    let pt = |x: f32, y: f32| pos2(c.x + (x - 12.0) * u, c.y + (y - 12.0) * u);
    let line = |pts: &[(f32, f32)]| {
        p.add(Shape::line(pts.iter().map(|&(x, y)| pt(x, y)).collect::<Vec<Pos2>>(), st));
    };
    let closed = |pts: &[(f32, f32)]| {
        p.add(Shape::closed_line(pts.iter().map(|&(x, y)| pt(x, y)).collect::<Vec<Pos2>>(), st));
    };
    let r = |x0: f32, y0: f32, x1: f32, y1: f32, rad: f32| {
        p.rect_stroke(Rect::from_min_max(pt(x0, y0), pt(x1, y1)), rad * u, st, StrokeKind::Middle);
    };
    match icon {
        Icon::Flash => closed(&[(13.5, 2.5), (5.5, 13.5), (11.2, 13.5), (10.0, 21.5), (18.5, 10.0), (12.6, 10.0)]),
        Icon::Boot => {
            // a boot menu: three entries, the first one selected
            r(3.5, 4.5, 20.5, 8.5, 1.5);
            line(&[(3.5, 12.5), (20.5, 12.5)]);
            line(&[(3.5, 16.5), (20.5, 16.5)]);
            line(&[(3.5, 20.5), (14.5, 20.5)]);
            p.circle_filled(pt(7.0, 6.5), 1.2 * u, color);
        }
        Icon::Format => {
            // a drive with its surface divided into blocks
            r(3.5, 3.5, 20.5, 20.5, 2.5);
            line(&[(12.0, 3.5), (12.0, 20.5)]);
            line(&[(3.5, 12.0), (20.5, 12.0)]);
        }
        Icon::Home => {
            closed(&[(3.5, 11.0), (12.0, 3.5), (20.5, 11.0), (20.5, 20.5), (14.5, 20.5), (14.5, 14.5), (9.5, 14.5), (9.5, 20.5), (3.5, 20.5)]);
        }
        Icon::Refresh => {
            let n = 22;
            let pts: Vec<(f32, f32)> = (0..=n)
                .map(|i| {
                    let a = (-30.0f32 + 300.0 * i as f32 / n as f32).to_radians();
                    (12.0 + 8.0 * a.cos(), 12.0 + 8.0 * a.sin())
                })
                .collect();
            line(&pts);
            let e = pts[n];
            line(&[(e.0 + 4.2, e.1 - 0.6), (e.0, e.1), (e.0 + 0.2, e.1 - 4.6)]);
        }
        Icon::Usb => {
            // the plug end of a USB stick
            r(7.0, 9.5, 17.0, 21.5, 2.0);
            r(9.0, 2.5, 15.0, 9.5, 0.5);
            p.circle_filled(pt(10.8, 5.8), 0.9 * u, color);
            p.circle_filled(pt(13.2, 5.8), 0.9 * u, color);
            line(&[(10.5, 15.0), (13.5, 15.0)]);
        }
        Icon::File => {
            closed(&[(5.5, 2.5), (14.0, 2.5), (19.0, 7.5), (19.0, 21.5), (5.5, 21.5)]);
            line(&[(14.0, 2.5), (14.0, 7.5), (19.0, 7.5)]);
        }
        Icon::Check => line(&[(4.5, 12.5), (9.8, 17.8), (19.5, 6.5)]),
        Icon::Warn => {
            closed(&[(12.0, 3.5), (21.5, 20.0), (2.5, 20.0)]);
            line(&[(12.0, 9.5), (12.0, 14.5)]);
            p.circle_filled(pt(12.0, 17.2), 0.95 * u, color);
        }
        Icon::Error => {
            p.circle_stroke(pt(12.0, 12.0), 9.0 * u, st);
            line(&[(8.5, 8.5), (15.5, 15.5)]);
            line(&[(15.5, 8.5), (8.5, 15.5)]);
        }
        Icon::Info => {
            p.circle_stroke(pt(12.0, 12.0), 9.0 * u, st);
            line(&[(12.0, 11.0), (12.0, 16.8)]);
            p.circle_filled(pt(12.0, 7.8), 0.95 * u, color);
        }
        Icon::Plus => {
            line(&[(12.0, 5.0), (12.0, 19.0)]);
            line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Icon::Trash => {
            line(&[(4.0, 6.5), (20.0, 6.5)]);
            line(&[(9.0, 6.5), (9.0, 4.0), (15.0, 4.0), (15.0, 6.5)]);
            closed(&[(6.0, 6.5), (18.0, 6.5), (17.0, 20.5), (7.0, 20.5)]);
            line(&[(10.0, 10.5), (10.0, 17.0)]);
            line(&[(14.0, 10.5), (14.0, 17.0)]);
        }
        Icon::Folder => closed(&[(3.0, 6.0), (9.5, 6.0), (11.5, 8.5), (21.0, 8.5), (21.0, 19.5), (3.0, 19.5)]),
        Icon::Back => {
            line(&[(20.0, 12.0), (4.5, 12.0)]);
            line(&[(10.5, 6.0), (4.5, 12.0), (10.5, 18.0)]);
        }
        Icon::Shield => {
            closed(&[(12.0, 2.5), (20.0, 5.5), (20.0, 12.0), (12.0, 21.5), (4.0, 12.0), (4.0, 5.5)]);
            line(&[(8.5, 11.5), (11.2, 14.2), (15.8, 9.0)]);
        }
    }
    let _ = vec2(0.0, 0.0);
}
