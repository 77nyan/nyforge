//! The NyForge mark, drawn as vectors: a USB stick on a blue tile with a spark.
//! The numbers match tools/make-logo.py (a 256 x 256 grid).

use eframe::egui::{epaint::PathShape, epaint::Mesh, pos2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

const TILE: (f32, f32, f32, f32, f32) = (8.0, 8.0, 248.0, 248.0, 56.0);
const PIVOT: (f32, f32) = (126.0, 136.0);
const ANGLE_DEG: f32 = 38.0;
const BODY: [f32; 5] = [-31.0, -30.0, 31.0, 66.0, 13.0]; // x0, y0, x1, y1, radius
const PLUG: [f32; 5] = [-22.0, -80.0, 22.0, -24.0, 4.0];
const PINS: [[f32; 5]; 2] = [[-13.0, -70.0, -5.0, -48.0, 1.5], [5.0, -70.0, 13.0, -48.0, 1.5]];
const HOLE: (f32, f32, f32) = (0.0, 47.0, 6.5);
const SPARK: (f32, f32, f32) = (207.0, 49.0, 14.0);
const SHADOW: (f32, f32) = (7.0, 11.0);

const C_TOP: Color32 = Color32::from_rgb(0x5C, 0xC0, 0xFF);
const C_BOT: Color32 = Color32::from_rgb(0x1C, 0x74, 0xD4);
const C_BODY: Color32 = Color32::WHITE;
const C_PLUG: Color32 = Color32::from_rgb(0xD3, 0xDD, 0xEA);
const C_PIN: Color32 = Color32::from_rgb(0x2F, 0x3B, 0x4E);
const C_HOLE: Color32 = Color32::from_rgb(0x2A, 0x7F, 0xDA);
const C_SPARK: Color32 = Color32::from_rgb(0xFF, 0xC8, 0x3D);

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Points of a rounded rectangle, clockwise.
fn rrect(r: [f32; 5], seg: usize) -> Vec<(f32, f32)> {
    let [x0, y0, x1, y1, rad] = r;
    let mut pts = Vec::with_capacity(4 * (seg + 1));
    for (cx, cy, a0) in [(x1 - rad, y0 + rad, -90.0_f32), (x1 - rad, y1 - rad, 0.0), (x0 + rad, y1 - rad, 90.0), (x0 + rad, y0 + rad, 180.0)] {
        for i in 0..=seg {
            let a = (a0 + 90.0 * i as f32 / seg as f32).to_radians();
            pts.push((cx + rad * a.cos(), cy + rad * a.sin()));
        }
    }
    pts
}

/// Turns points drawn around the pivot by the stick's angle.
fn turn(pts: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let a = ANGLE_DEG.to_radians();
    let (ca, sa) = (a.cos(), a.sin());
    pts.iter().map(|&(x, y)| (PIVOT.0 + x * ca - y * sa, PIVOT.1 + x * sa + y * ca)).collect()
}

/// Draws the logo into `rect`. With `tile` it sits on its rounded blue tile; without, only the stick and spark are drawn.
pub fn paint(p: &Painter, rect: Rect, tile: bool) {
    let s = rect.width() / 256.0;
    let pt = |x: f32, y: f32| pos2(rect.left() + x * s, rect.top() + y * s);
    let poly = |pts: &[(f32, f32)], fill: Color32| -> Shape {
        Shape::Path(PathShape::convex_polygon(pts.iter().map(|&(x, y)| pt(x, y)).collect::<Vec<Pos2>>(), fill, Stroke::NONE))
    };
    if tile {
        // vertical gradient: a triangle fan over the rounded-rect outline, coloured by height
        let outline = rrect([TILE.0, TILE.1, TILE.2, TILE.3, TILE.4], 14);
        let mut mesh = Mesh::default();
        mesh.colored_vertex(pt(128.0, 128.0), lerp(C_TOP, C_BOT, 0.5));
        for &(x, y) in &outline {
            mesh.colored_vertex(pt(x, y), lerp(C_TOP, C_BOT, (y - TILE.1) / (TILE.3 - TILE.1)));
        }
        let n = outline.len() as u32;
        for i in 0..n {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
        }
        p.add(Shape::mesh(mesh));
        if rect.width() >= 40.0 {
            let r = Rect::from_min_max(pt(TILE.0, TILE.1), pt(TILE.2, TILE.3));
            p.rect_stroke(r, TILE.4 * s, Stroke::new((2.5 * s).max(1.0), Color32::from_white_alpha(46)), StrokeKind::Inside);
        }
    }
    // a soft shadow: a few offset, translucent copies
    for (grow, alpha) in [(3.0_f32, 14u8), (1.5, 18), (0.0, 22)] {
        for shape in [BODY, PLUG] {
            let g = [shape[0] - grow, shape[1] - grow, shape[2] + grow, shape[3] + grow, shape[4] + grow];
            let pts: Vec<(f32, f32)> = turn(&rrect(g, 8)).into_iter().map(|(x, y)| (x + SHADOW.0, y + SHADOW.1)).collect();
            p.add(poly(&pts, Color32::from_rgba_unmultiplied(8, 40, 100, alpha)));
        }
    }
    p.add(poly(&turn(&rrect(PLUG, 6)), C_PLUG));
    p.add(poly(&turn(&rrect(BODY, 10)), C_BODY));
    for pin in PINS {
        p.add(poly(&turn(&rrect(pin, 3)), C_PIN));
    }
    let (hx, hy) = turn(&[(HOLE.0, HOLE.1)])[0];
    p.circle_filled(pt(hx, hy), HOLE.2 * s, C_HOLE);
    // the spark: a four-point star as a triangle fan
    let (cx, cy, r) = SPARK;
    let k = r * 0.28;
    let ring = [(cx, cy - r), (cx + k, cy - k), (cx + r, cy), (cx + k, cy + k), (cx, cy + r), (cx - k, cy + k), (cx - r, cy), (cx - k, cy - k)];
    let mut m = Mesh::default();
    m.colored_vertex(pt(cx, cy), C_SPARK);
    for (x, y) in ring {
        m.colored_vertex(pt(x, y), C_SPARK);
    }
    for i in 0..8u32 {
        m.add_triangle(0, 1 + i, 1 + (i + 1) % 8);
    }
    p.add(Shape::mesh(m));
}
