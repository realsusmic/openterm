//! icons — tiny vector icons painted directly, so no glyph ever renders as □.
use egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Rounding, Shape, Stroke};

fn s(c: Color32) -> Stroke {
    Stroke::new(1.3_f32, c)
}

pub fn chevron(p: &Painter, c: Pos2, open: bool, col: Color32) {
    let pts = if open {
        vec![
            pos2(c.x - 3.0, c.y - 1.5),
            pos2(c.x, c.y + 1.5),
            pos2(c.x + 3.0, c.y - 1.5),
        ]
    } else {
        vec![
            pos2(c.x - 1.5, c.y - 3.0),
            pos2(c.x + 1.5, c.y),
            pos2(c.x - 1.5, c.y + 3.0),
        ]
    };
    p.add(Shape::line(pts, s(col)));
}

pub fn folder(p: &Painter, c: Pos2, col: Color32) {
    let body = Rect::from_center_size(c + vec2(0.0, 0.8), vec2(12.0, 8.5));
    p.rect_stroke(body, Rounding::same(1.5), s(col));
    p.line_segment(
        [
            pos2(body.left() + 1.0, body.top()),
            pos2(body.left() + 1.0, body.top() - 1.8),
        ],
        s(col),
    );
    p.line_segment(
        [
            pos2(body.left() + 1.0, body.top() - 1.8),
            pos2(body.left() + 5.0, body.top() - 1.8),
        ],
        s(col),
    );
}

pub fn file(p: &Painter, c: Pos2, col: Color32) {
    let r = Rect::from_center_size(c, vec2(8.5, 11.0));
    p.rect_stroke(r, Rounding::same(1.5), s(col));
    p.line_segment(
        [
            pos2(r.left() + 2.2, c.y - 1.0),
            pos2(r.right() - 2.2, c.y - 1.0),
        ],
        Stroke::new(1.0_f32, col),
    );
    p.line_segment(
        [
            pos2(r.left() + 2.2, c.y + 1.8),
            pos2(r.right() - 2.2, c.y + 1.8),
        ],
        Stroke::new(1.0_f32, col),
    );
}

pub fn laptop(p: &Painter, c: Pos2, col: Color32) {
    let scr = Rect::from_center_size(c + vec2(0.0, -1.2), vec2(10.0, 7.0));
    p.rect_stroke(scr, Rounding::same(1.2), s(col));
    p.line_segment(
        [
            pos2(c.x - 6.5, scr.bottom() + 2.2),
            pos2(c.x + 6.5, scr.bottom() + 2.2),
        ],
        s(col),
    );
}

pub fn server(p: &Painter, c: Pos2, col: Color32) {
    for dy in [-2.6, 2.6] {
        let r = Rect::from_center_size(c + vec2(0.0, dy), vec2(11.0, 4.6));
        p.rect_stroke(r, Rounding::same(1.2), s(col));
        p.circle_filled(pos2(r.left() + 2.4, r.center().y), 0.9, col);
    }
}

pub fn prompt(p: &Painter, c: Pos2, col: Color32) {
    p.add(Shape::line(
        vec![
            pos2(c.x - 5.0, c.y - 3.0),
            pos2(c.x - 2.0, c.y),
            pos2(c.x - 5.0, c.y + 3.0),
        ],
        s(col),
    ));
    p.line_segment([pos2(c.x, c.y + 3.2), pos2(c.x + 5.0, c.y + 3.2)], s(col));
}

pub fn plus(p: &Painter, c: Pos2, r: f32, col: Color32) {
    p.line_segment([pos2(c.x - r, c.y), pos2(c.x + r, c.y)], s(col));
    p.line_segment([pos2(c.x, c.y - r), pos2(c.x, c.y + r)], s(col));
}

pub fn cross(p: &Painter, c: Pos2, r: f32, col: Color32) {
    p.line_segment([pos2(c.x - r, c.y - r), pos2(c.x + r, c.y + r)], s(col));
    p.line_segment([pos2(c.x - r, c.y + r), pos2(c.x + r, c.y - r)], s(col));
}

pub fn minimize(p: &Painter, c: Pos2, col: Color32) {
    p.line_segment(
        [pos2(c.x - 5.0, c.y), pos2(c.x + 5.0, c.y)],
        Stroke::new(1.0_f32, col),
    );
}

pub fn maximize(p: &Painter, c: Pos2, restored: bool, col: Color32) {
    if restored {
        p.rect_stroke(
            Rect::from_center_size(c, vec2(9.0, 9.0)),
            Rounding::same(1.0),
            Stroke::new(1.0_f32, col),
        );
    } else {
        p.rect_stroke(
            Rect::from_center_size(c + vec2(-1.2, 1.2), vec2(7.5, 7.5)),
            Rounding::same(1.0),
            Stroke::new(1.0_f32, col),
        );
        p.add(Shape::line(
            vec![
                pos2(c.x - 2.4, c.y - 3.6),
                pos2(c.x + 3.6, c.y - 3.6),
                pos2(c.x + 3.6, c.y + 2.4),
            ],
            Stroke::new(1.0_f32, col),
        ));
    }
}

pub fn split(p: &Painter, c: Pos2, col: Color32) {
    let r = Rect::from_center_size(c, vec2(13.0, 10.0));
    p.rect_stroke(r, Rounding::same(2.0), s(col));
    p.line_segment(
        [pos2(c.x + 1.0, r.top()), pos2(c.x + 1.0, r.bottom())],
        s(col),
    );
}

pub fn up(p: &Painter, c: Pos2, col: Color32) {
    p.line_segment([pos2(c.x, c.y + 4.5), pos2(c.x, c.y - 4.0)], s(col));
    p.add(Shape::line(
        vec![
            pos2(c.x - 3.5, c.y - 0.5),
            pos2(c.x, c.y - 4.0),
            pos2(c.x + 3.5, c.y - 0.5),
        ],
        s(col),
    ));
}

pub fn refresh(p: &Painter, c: Pos2, col: Color32) {
    let r = 4.5;
    let pts: Vec<Pos2> = (0..=20)
        .map(|i| {
            let a = 0.6 + (i as f32 / 20.0) * 5.0;
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();
    let end = *pts.last().unwrap();
    p.add(Shape::line(pts, s(col)));
    p.add(Shape::line(
        vec![
            pos2(end.x - 2.8, end.y - 0.6),
            end,
            pos2(end.x - 0.2, end.y + 2.8),
        ],
        s(col),
    ));
}

pub fn gear(p: &Painter, c: Pos2, col: Color32) {
    p.circle_stroke(c, 4.0, Stroke::new(1.25_f32, col));
    p.circle_stroke(c, 1.4, Stroke::new(1.1_f32, col));
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = vec2(a.cos(), a.sin());
        p.line_segment([c + dir * 4.5, c + dir * 6.0], Stroke::new(1.25_f32, col));
    }
}

pub fn dot(p: &Painter, c: Pos2, r: f32, col: Color32) {
    p.circle_filled(c, r, col);
}

/// Inlined from the supplied cf-icon-svg information mark.
pub fn info(p: &Painter, c: Pos2, col: Color32) {
    p.circle_stroke(c, 5.4, Stroke::new(1.25_f32, col));
    p.circle_filled(c + vec2(0.0, -2.5), 0.8, col);
    p.line_segment(
        [c + vec2(0.0, -0.2), c + vec2(0.0, 3.0)],
        Stroke::new(1.5_f32, col),
    );
}

/// Inlined rendition of powershell-svgrepo-com.svg.
pub fn powershell(p: &Painter, c: Pos2, col: Color32) {
    p.add(Shape::closed_line(
        vec![
            c + vec2(-5.6, -4.7),
            c + vec2(6.0, -4.7),
            c + vec2(4.3, 4.7),
            c + vec2(-6.0, 4.7),
        ],
        Stroke::new(1.35_f32, col),
    ));
    p.add(Shape::line(
        vec![
            c + vec2(-3.0, -2.7),
            c + vec2(0.3, -0.1),
            c + vec2(-3.4, 2.6),
        ],
        Stroke::new(1.6_f32, col),
    ));
    p.line_segment(
        [c + vec2(0.5, 2.7), c + vec2(3.4, 2.7)],
        Stroke::new(1.45_f32, col),
    );
}

/// Inlined rendition of the supplied bash.svg terminal and dollar prompt.
pub fn bash(p: &Painter, c: Pos2, col: Color32) {
    let frame = Rect::from_center_size(c, vec2(12.0, 11.0));
    p.rect_stroke(frame, Rounding::same(2.2), Stroke::new(1.25_f32, col));
    p.add(Shape::line(
        vec![
            c + vec2(-2.6, -2.5),
            c + vec2(-4.0, -3.0),
            c + vec2(-4.6, -1.5),
            c + vec2(-2.7, -0.3),
            c + vec2(-3.1, 1.5),
            c + vec2(-4.8, 1.0),
        ],
        Stroke::new(1.15_f32, col),
    ));
    p.line_segment(
        [c + vec2(-3.6, -3.8), c + vec2(-3.6, 2.4)],
        Stroke::new(1.0_f32, col),
    );
    p.line_segment(
        [c + vec2(0.1, 2.0), c + vec2(3.7, 2.0)],
        Stroke::new(1.3_f32, col),
    );
}

/// Inlined rendition of the supplied wsl.svg Tux mark.
pub fn wsl(p: &Painter, c: Pos2, col: Color32) {
    let thin = Stroke::new(0.9_f32, col);
    let outline = Stroke::new(1.15_f32, col);

    // Tall, flat-bottomed Tux silhouette from the supplied 500x664 SVG.
    p.add(Shape::closed_line(
        vec![
            c + vec2(0.0, -6.2),
            c + vec2(-2.8, -5.8),
            c + vec2(-4.8, -4.0),
            c + vec2(-5.4, -1.2),
            c + vec2(-5.4, 5.8),
            c + vec2(5.4, 5.8),
            c + vec2(5.4, -1.2),
            c + vec2(4.8, -4.0),
            c + vec2(2.8, -5.8),
        ],
        outline,
    ));

    // Inner face: the two eye lobes join into the rounded lower mask.
    p.add(Shape::line(
        vec![
            c + vec2(-4.0, 1.5),
            c + vec2(-4.0, -2.1),
            c + vec2(-3.2, -3.5),
            c + vec2(-2.0, -3.6),
            c + vec2(-0.6, -2.5),
            c + vec2(-0.6, -0.1),
            c + vec2(0.0, -0.3),
            c + vec2(0.6, -0.1),
            c + vec2(0.6, -2.5),
            c + vec2(2.0, -3.6),
            c + vec2(3.2, -3.5),
            c + vec2(4.0, -2.1),
            c + vec2(4.0, 1.5),
            c + vec2(3.3, 3.5),
            c + vec2(0.0, 4.5),
            c + vec2(-3.3, 3.5),
            c + vec2(-4.0, 1.5),
        ],
        thin,
    ));
    p.circle_stroke(c + vec2(-2.1, -1.7), 0.85, thin);
    p.circle_stroke(c + vec2(2.1, -1.7), 0.85, thin);
    p.add(Shape::ellipse_stroke(
        c + vec2(0.0, 1.2),
        vec2(2.0, 1.8),
        thin,
    ));
    p.add(Shape::line(
        vec![c + vec2(-1.9, 1.2), c + vec2(0.0, 1.7), c + vec2(1.9, 1.2)],
        thin,
    ));
    p.add(Shape::line(
        vec![c + vec2(-4.0, 1.5), c + vec2(0.0, 4.5), c + vec2(4.0, 1.5)],
        thin,
    ));
}

/// Inlined rendition of the supplied cmd.svg terminal window.
pub fn cmd(p: &Painter, c: Pos2, col: Color32) {
    let frame = Rect::from_center_size(c, vec2(12.0, 10.0));
    p.rect_stroke(frame, Rounding::same(2.0), Stroke::new(1.25_f32, col));
    p.add(Shape::line(
        vec![
            c + vec2(-3.5, -2.2),
            c + vec2(-1.2, 0.0),
            c + vec2(-3.5, 2.2),
        ],
        Stroke::new(1.35_f32, col),
    ));
    p.line_segment(
        [c + vec2(0.2, 2.1), c + vec2(3.3, 2.1)],
        Stroke::new(1.35_f32, col),
    );
}

/// Native egui renditions of the supplied Windows, Linux and macOS SVGs.
/// Keeping these paths in Rust means the status icon works without filesystem
/// assets on every platform OpenTerm ships on.
pub fn platform(p: &Painter, c: Pos2, name: &str, col: Color32) {
    let name = name.to_ascii_lowercase();
    if name.contains("windows") || name.contains("mingw") || name.contains("cygwin") {
        windows(p, c, col);
    } else if name.contains("darwin") || name.contains("macos") || name.contains("mac os") {
        macos(p, c, col);
    } else if name.contains("linux") {
        linux(p, c, col);
    } else {
        server(p, c, col);
    }
}

fn windows(p: &Painter, c: Pos2, col: Color32) {
    // Four skewed panes from windows-174-svgrepo-com.svg.
    let gap = 0.7;
    let x0 = c.x - 5.5;
    let x1 = c.x - gap / 2.0;
    let x2 = c.x + gap / 2.0;
    let x3 = c.x + 5.5;
    let top_l = c.y - 4.2;
    let top_r = c.y - 5.4;
    let bot_l = c.y + 4.2;
    let bot_r = c.y + 5.4;
    for points in [
        vec![
            pos2(x0, top_l),
            pos2(x1, c.y - 4.8),
            pos2(x1, c.y - gap / 2.0),
            pos2(x0, c.y - gap / 2.0),
        ],
        vec![
            pos2(x2, c.y - 4.9),
            pos2(x3, top_r),
            pos2(x3, c.y - gap / 2.0),
            pos2(x2, c.y - gap / 2.0),
        ],
        vec![
            pos2(x0, c.y + gap / 2.0),
            pos2(x1, c.y + gap / 2.0),
            pos2(x1, c.y + 4.8),
            pos2(x0, bot_l),
        ],
        vec![
            pos2(x2, c.y + gap / 2.0),
            pos2(x3, c.y + gap / 2.0),
            pos2(x3, bot_r),
            pos2(x2, c.y + 4.9),
        ],
    ] {
        p.add(Shape::convex_polygon(points, col, Stroke::NONE));
    }
}

fn linux(p: &Painter, c: Pos2, col: Color32) {
    // Compact Tux silhouette based on linux-svgrepo-com.svg.
    p.add(Shape::convex_polygon(
        vec![
            pos2(c.x - 4.0, c.y + 4.7),
            pos2(c.x - 3.0, c.y - 1.8),
            pos2(c.x - 1.8, c.y - 4.8),
            pos2(c.x, c.y - 5.5),
            pos2(c.x + 1.8, c.y - 4.8),
            pos2(c.x + 3.0, c.y - 1.8),
            pos2(c.x + 4.0, c.y + 4.7),
        ],
        col,
        Stroke::NONE,
    ));
    let cut = Color32::from_rgba_unmultiplied(0, 0, 0, 150);
    p.add(Shape::ellipse_filled(
        c + vec2(0.0, 1.7),
        vec2(2.4, 3.0),
        cut,
    ));
    p.circle_filled(c + vec2(-1.1, -2.7), 0.65, cut);
    p.circle_filled(c + vec2(1.1, -2.7), 0.65, cut);
    p.line_segment(
        [c + vec2(-4.8, 5.1), c + vec2(-0.8, 4.4)],
        Stroke::new(1.5_f32, col),
    );
    p.line_segment(
        [c + vec2(0.8, 4.4), c + vec2(4.8, 5.1)],
        Stroke::new(1.5_f32, col),
    );
}

fn macos(p: &Painter, c: Pos2, col: Color32) {
    // Finder face distilled from macos-svgrepo-com.svg.
    let r = Rect::from_center_size(c, vec2(11.0, 11.0));
    p.rect_stroke(r, Rounding::same(2.2), Stroke::new(1.2_f32, col));
    p.line_segment(
        [pos2(c.x, r.top()), pos2(c.x - 0.7, c.y + 1.5)],
        Stroke::new(1.0_f32, col),
    );
    p.circle_filled(c + vec2(-2.0, -1.6), 0.65, col);
    p.circle_filled(c + vec2(2.0, -1.6), 0.65, col);
    p.add(Shape::line(
        vec![
            c + vec2(-2.9, 2.0),
            c + vec2(-1.3, 3.0),
            c + vec2(1.4, 3.0),
            c + vec2(3.0, 1.7),
        ],
        Stroke::new(1.0_f32, col),
    ));
}
