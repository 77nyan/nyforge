//! Simple line icons drawn with the painter, so they stay crisp at every size and match the text colour.

use eframe::egui::{epaint::Mesh, pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

fn with_a(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

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
    Test,
}

/// Paints `icon` centred in `rect` with line width scaled to the size.
pub fn paint(p: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let c = rect.center();
    let u = rect.width().min(rect.height()) / 24.0; // the icons are designed on a 24-unit grid
    let w = (1.7 * u).max(1.0);
    let st = Stroke::new(w, color);
    // a soft tint inside closed shapes, so the icons have a little body (a lighter touch on small sizes)
    let tint = with_a(color, if u >= 0.9 { 34 } else { 22 });
    let pt = |x: f32, y: f32| pos2(c.x + (x - 12.0) * u, c.y + (y - 12.0) * u);
    // Round caps and joins: a dot of the stroke's width on every vertex.
    let dots = |pts: &[Pos2]| {
        if w >= 1.6 {
            for q in pts {
                p.circle_filled(*q, w / 2.0, color);
            }
        }
    };
    let line = |pts: &[(f32, f32)]| {
        let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
        p.add(Shape::line(v.clone(), st));
        dots(&v);
    };
    // Convex polygon with a tint and an outline.
    let closed = |pts: &[(f32, f32)]| {
        let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
        p.add(Shape::closed_line(v.clone(), st));
        dots(&v);
    };
    let closed_tint = |pts: &[(f32, f32)]| {
        let v: Vec<Pos2> = pts.iter().map(|&(x, y)| pt(x, y)).collect();
        p.add(Shape::convex_polygon(v.clone(), tint, Stroke::NONE));
        p.add(Shape::closed_line(v.clone(), st));
        dots(&v);
    };
    // Triangles for shapes that are not convex (the bolt).
    let tris = |tri: &[[(f32, f32); 3]]| {
        let mut m = Mesh::default();
        for t in tri {
            let i = m.vertices.len() as u32;
            for &(x, y) in t {
                m.colored_vertex(pt(x, y), tint);
            }
            m.add_triangle(i, i + 1, i + 2);
        }
        p.add(Shape::mesh(m));
    };
    let r = |x0: f32, y0: f32, x1: f32, y1: f32, rad: f32| {
        let rect = Rect::from_min_max(pt(x0, y0), pt(x1, y1));
        p.rect_filled(rect, rad * u, tint);
        p.rect_stroke(rect, rad * u, st, StrokeKind::Middle);
    };
    let r_plain = |x0: f32, y0: f32, x1: f32, y1: f32, rad: f32| {
        p.rect_stroke(Rect::from_min_max(pt(x0, y0), pt(x1, y1)), rad * u, st, StrokeKind::Middle);
    };
    match icon {
        Icon::Flash => {
            tris(&[[(13.5, 2.5), (5.5, 13.5), (12.6, 10.0)], [(5.5, 13.5), (11.2, 13.5), (12.6, 10.0)], [(11.2, 13.5), (18.5, 10.0), (12.6, 10.0)], [(11.2, 13.5), (10.0, 21.5), (18.5, 10.0)]]);
            closed(&[(13.5, 2.5), (5.5, 13.5), (11.2, 13.5), (10.0, 21.5), (18.5, 10.0), (12.6, 10.0)]);
        }
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
            tris(&[
                [(3.5, 11.0), (12.0, 3.5), (20.5, 11.0)],
                [(3.5, 11.0), (20.5, 11.0), (14.5, 14.5)],
                [(3.5, 11.0), (14.5, 14.5), (9.5, 14.5)],
                [(3.5, 11.0), (9.5, 14.5), (9.5, 20.5)],
                [(3.5, 11.0), (9.5, 20.5), (3.5, 20.5)],
                [(20.5, 11.0), (20.5, 20.5), (14.5, 20.5)],
                [(20.5, 11.0), (14.5, 20.5), (14.5, 14.5)],
            ]);
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
            closed_tint(&[(5.5, 2.5), (14.0, 2.5), (19.0, 7.5), (19.0, 21.5), (5.5, 21.5)]);
            line(&[(14.0, 2.5), (14.0, 7.5), (19.0, 7.5)]);
        }
        Icon::Check => line(&[(4.5, 12.5), (9.8, 17.8), (19.5, 6.5)]),
        Icon::Warn => {
            closed_tint(&[(12.0, 3.5), (21.5, 20.0), (2.5, 20.0)]);
            line(&[(12.0, 9.5), (12.0, 14.5)]);
            p.circle_filled(pt(12.0, 17.2), 0.95 * u, color);
        }
        Icon::Error => {
            p.circle_filled(pt(12.0, 12.0), 9.0 * u, tint);
            p.circle_stroke(pt(12.0, 12.0), 9.0 * u, st);
            line(&[(8.5, 8.5), (15.5, 15.5)]);
            line(&[(15.5, 8.5), (8.5, 15.5)]);
        }
        Icon::Info => {
            p.circle_filled(pt(12.0, 12.0), 9.0 * u, tint);
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
            closed_tint(&[(6.0, 6.5), (18.0, 6.5), (17.0, 20.5), (7.0, 20.5)]);
            line(&[(10.0, 10.5), (10.0, 17.0)]);
            line(&[(14.0, 10.5), (14.0, 17.0)]);
        }
        Icon::Folder => {
            tris(&[[(3.0, 6.0), (9.5, 6.0), (3.0, 19.5)], [(9.5, 6.0), (11.5, 8.5), (3.0, 19.5)], [(11.5, 8.5), (21.0, 19.5), (3.0, 19.5)], [(11.5, 8.5), (21.0, 8.5), (21.0, 19.5)]]);
            closed(&[(3.0, 6.0), (9.5, 6.0), (11.5, 8.5), (21.0, 8.5), (21.0, 19.5), (3.0, 19.5)]);
        }
        Icon::Back => {
            line(&[(20.0, 12.0), (4.5, 12.0)]);
            line(&[(10.5, 6.0), (4.5, 12.0), (10.5, 18.0)]);
        }
        Icon::Test => {
            // a gauge: dial, ticks and a needle
            let n = 20;
            let pts: Vec<(f32, f32)> = (0..=n)
                .map(|i| {
                    let a = (180.0f32 + 180.0 * i as f32 / n as f32).to_radians();
                    (12.0 + 9.0 * a.cos(), 15.0 + 9.0 * a.sin())
                })
                .collect();
            line(&pts);
            line(&[(3.0, 15.0), (21.0, 15.0)]);
            line(&[(12.0, 15.0), (16.6, 8.6)]);
            p.circle_filled(pt(12.0, 15.0), 1.5 * u, color);
            line(&[(12.0, 6.6), (12.0, 8.2)]);
            line(&[(5.7, 9.3), (6.9, 10.3)]);
            line(&[(18.3, 9.3), (17.1, 10.3)]);
            line(&[(6.5, 19.0), (17.5, 19.0)]);
        }
        Icon::Shield => {
            closed_tint(&[(12.0, 2.5), (20.0, 5.5), (20.0, 12.0), (12.0, 21.5), (4.0, 12.0), (4.0, 5.5)]);
            line(&[(8.5, 11.5), (11.2, 14.2), (15.8, 9.0)]);
        }
    }
    let _ = (vec2(0.0, 0.0), &r_plain);
}

/// A plain cog: one outline with six chunky teeth and a round hole. No fill, no turning.
pub fn gear(p: &Painter, c: Pos2, r: f32, color: Color32) {
    let st = Stroke::new((r * 0.2).max(1.4), color);
    let teeth = 6;
    let step = std::f32::consts::TAU / teeth as f32;
    let (r_in, r_out) = (r * 0.72, r);
    let mut pts = Vec::new();
    for k in 0..teeth {
        let a0 = step * k as f32 - std::f32::consts::FRAC_PI_2;
        // root, flank up, flat top, flank down: the tooth is about a third of the step wide
        for (da, rad) in [(-0.40, r_in), (-0.17, r_out), (0.17, r_out), (0.40, r_in)] {
            let a = a0 + da * step;
            pts.push(pos2(c.x + rad * a.cos(), c.y + rad * a.sin()));
        }
    }
    pts.push(pts[0]);
    p.add(Shape::line(pts, st));
    p.circle_stroke(c, r * 0.30, st);
}
