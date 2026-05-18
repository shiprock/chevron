use image::{ImageBuffer, ImageEncoder, Rgba, RgbaImage};
use std::io::{self, Write};

use super::font;
use crate::sysinfo::SystemInfo;

const COLS: u32 = 80;
const ROWS: u32 = 25;
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 16;

// CP437 box-drawing / shade characters
const BOX_HZ: u8 = 0xC4; // ─
const BOX_TL: u8 = 0xC9; // ╔
const BOX_TR: u8 = 0xBB; // ╗
const BOX_BL: u8 = 0xC8; // ╚
const BOX_BR: u8 = 0xBC; // ╝
const BOX_VT: u8 = 0xBA; // ║
const BOX_DB: u8 = 0xCD; // ═
const BOX_RT: u8 = 0xB5; // ╡
const BOX_LT: u8 = 0xC6; // ╞
const BULLET: u8 = 0xF9; // ∙
const DIAMOND: u8 = 0x04; // ♦

struct Palette {
    bg: Rgba<u8>,
    header_fg: Rgba<u8>,
    border_fg: Rgba<u8>,
    label_fg: Rgba<u8>,
    value_fg: Rgba<u8>,
    gradient: [Rgba<u8>; 4],
    title_face: Rgba<u8>,
    title_hi: Rgba<u8>,
    title_shadow: Rgba<u8>,
    bar_warn: Rgba<u8>,
    bar_crit: Rgba<u8>,
}

fn rgba(r: u8, g: u8, b: u8) -> Rgba<u8> {
    Rgba([r, g, b, 255])
}

fn palette_by_name(name: &str) -> Palette {
    match name {
        "fire" => Palette {
            bg: rgba(20, 5, 0),
            header_fg: rgba(255, 100, 20),
            border_fg: rgba(180, 60, 10),
            label_fg: rgba(200, 80, 15),
            value_fg: rgba(255, 180, 60),
            gradient: [
                rgba(80, 20, 0),
                rgba(160, 50, 5),
                rgba(220, 80, 10),
                rgba(255, 140, 30),
            ],
            title_face: rgba(210, 70, 8),
            title_hi: rgba(255, 230, 120),
            title_shadow: rgba(30, 5, 0),
            bar_warn: rgba(255, 180, 40),
            bar_crit: rgba(255, 60, 30),
        },
        "matrix" => Palette {
            bg: rgba(0, 10, 0),
            header_fg: rgba(0, 255, 65),
            border_fg: rgba(0, 140, 35),
            label_fg: rgba(0, 180, 45),
            value_fg: rgba(80, 255, 120),
            gradient: [
                rgba(0, 40, 10),
                rgba(0, 100, 25),
                rgba(0, 180, 45),
                rgba(0, 255, 65),
            ],
            title_face: rgba(0, 170, 42),
            title_hi: rgba(180, 255, 200),
            title_shadow: rgba(0, 18, 4),
            bar_warn: rgba(200, 255, 50),
            bar_crit: rgba(255, 60, 40),
        },
        "steel" => Palette {
            bg: rgba(15, 18, 22),
            header_fg: rgba(200, 210, 220),
            border_fg: rgba(100, 110, 120),
            label_fg: rgba(140, 150, 160),
            value_fg: rgba(220, 225, 230),
            gradient: [
                rgba(50, 55, 65),
                rgba(90, 95, 105),
                rgba(140, 150, 160),
                rgba(200, 210, 220),
            ],
            title_face: rgba(150, 160, 175),
            title_hi: rgba(235, 240, 250),
            title_shadow: rgba(18, 20, 30),
            bar_warn: rgba(230, 200, 80),
            bar_crit: rgba(220, 80, 60),
        },
        // "cyber" and default
        _ => Palette {
            bg: rgba(8, 4, 20),
            header_fg: rgba(213, 110, 255),
            border_fg: rgba(99, 60, 180),
            label_fg: rgba(141, 80, 200),
            value_fg: rgba(177, 160, 255),
            gradient: [
                rgba(57, 20, 100),
                rgba(93, 40, 160),
                rgba(141, 70, 220),
                rgba(213, 110, 255),
            ],
            title_face: rgba(130, 60, 210),
            title_hi: rgba(240, 210, 255),
            title_shadow: rgba(18, 5, 40),
            bar_warn: rgba(255, 200, 60),
            bar_crit: rgba(255, 50, 50),
        },
    }
}

// ---------------------------------------------------------------------------
// Character-level drawing (for text, boxes, bars)
// ---------------------------------------------------------------------------

fn draw_char(
    img: &mut RgbaImage,
    col: u32,
    row: u32,
    ch: u8,
    fg: Rgba<u8>,
    bg: Rgba<u8>,
    scale: u32,
) {
    let x0 = col * GLYPH_W * scale;
    let y0 = row * GLYPH_H * scale;
    for py in 0..GLYPH_H {
        for px in 0..GLYPH_W {
            let color = if font::glyph_pixel(ch, px, py) {
                fg
            } else {
                bg
            };
            for sy in 0..scale {
                for sx in 0..scale {
                    let ix = x0 + px * scale + sx;
                    let iy = y0 + py * scale + sy;
                    if ix < img.width() && iy < img.height() {
                        img.put_pixel(ix, iy, color);
                    }
                }
            }
        }
    }
}

fn draw_bytes(
    img: &mut RgbaImage,
    col: u32,
    row: u32,
    data: &[u8],
    fg: Rgba<u8>,
    bg: Rgba<u8>,
    scale: u32,
) {
    for (i, &ch) in data.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let c = col + i as u32;
        if c < COLS {
            draw_char(img, c, row, ch, fg, bg, scale);
        }
    }
}

fn draw_text(
    img: &mut RgbaImage,
    col: u32,
    row: u32,
    text: &str,
    fg: Rgba<u8>,
    bg: Rgba<u8>,
    scale: u32,
) {
    draw_bytes(img, col, row, text.as_bytes(), fg, bg, scale);
}

fn draw_gradient_bar(
    img: &mut RgbaImage,
    row: u32,
    gradient: &[Rgba<u8>; 4],
    reverse: bool,
    scale: u32,
) {
    // CP437: 0xB0=░ 0xB1=▒ 0xB2=▓ 0xDB=█
    let chars: [u8; 4] = if reverse {
        [0xDB, 0xB2, 0xB1, 0xB0]
    } else {
        [0xB0, 0xB1, 0xB2, 0xDB]
    };
    let bg = Rgba([0, 0, 0, 255]);
    for col in 0..COLS {
        let gi = (col as usize * 4 / COLS as usize).min(3);
        let ci = col as usize % 4;
        draw_char(img, col, row, chars[ci], gradient[gi], bg, scale);
    }
}

/// Draw text with a shadow on the row below for a bolder look.
fn draw_block_text(
    img: &mut RgbaImage,
    col: u32,
    row: u32,
    text: &str,
    fg: Rgba<u8>,
    bg: Rgba<u8>,
    scale: u32,
) {
    let dim = Rgba([fg.0[0] / 3, fg.0[1] / 3, fg.0[2] / 3, 255]);
    for (i, ch) in text.bytes().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let c = col + i as u32;
        if c < COLS {
            draw_char(img, c, row, ch, fg, bg, scale);
            draw_char(img, c, row + 1, ch, dim, bg, scale);
        }
    }
}

// ---------------------------------------------------------------------------
// Pixel-level drawing helpers (for 3D title)
// ---------------------------------------------------------------------------

fn fill_rect(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: Rgba<u8>) {
    let x_end = (x + w).min(img.width());
    let y_end = (y + h).min(img.height());
    for py in y..y_end {
        for px in x..x_end {
            img.put_pixel(px, py, color);
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn lerp_color(a: Rgba<u8>, b: Rgba<u8>, t: f32) -> Rgba<u8> {
    let mix = |a: u8, b: u8| -> u8 {
        f32::from(a)
            .mul_add(1.0 - t, f32::from(b) * t)
            .clamp(0.0, 255.0) as u8
    };
    Rgba([
        mix(a.0[0], b.0[0]),
        mix(a.0[1], b.0[1]),
        mix(a.0[2], b.0[2]),
        255,
    ])
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn scale_color(c: Rgba<u8>, factor: f32) -> Rgba<u8> {
    Rgba([
        (f32::from(c.0[0]) * factor).clamp(0.0, 255.0) as u8,
        (f32::from(c.0[1]) * factor).clamp(0.0, 255.0) as u8,
        (f32::from(c.0[2]) * factor).clamp(0.0, 255.0) as u8,
        255,
    ])
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let d = edge1 - edge0;
    if d.abs() < 1e-6 {
        return if x >= edge0 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / d).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn blend_pixel(img: &mut RgbaImage, x: u32, y: u32, color: Rgba<u8>, alpha: f32) {
    if x >= img.width() || y >= img.height() || alpha <= 0.0 {
        return;
    }
    let bg = *img.get_pixel(x, y);
    let a = alpha.clamp(0.0, 1.0);
    let mix = |fg: u8, bg: u8| -> u8 {
        f32::from(fg)
            .mul_add(a, f32::from(bg) * (1.0 - a))
            .clamp(0.0, 255.0) as u8
    };
    img.put_pixel(
        x,
        y,
        Rgba([
            mix(color.0[0], bg.0[0]),
            mix(color.0[1], bg.0[1]),
            mix(color.0[2], bg.0[2]),
            255,
        ]),
    );
}

#[allow(clippy::cast_precision_loss, clippy::many_single_char_names)]
fn fill_rect_vgradient(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    top: Rgba<u8>,
    bot: Rgba<u8>,
) {
    if h == 0 {
        return;
    }
    let x_end = (x + w).min(img.width());
    let y_end = (y + h).min(img.height());
    let denom = (h - 1).max(1) as f32;
    for dy in 0..h {
        if y + dy >= y_end {
            break;
        }
        let t = dy as f32 / denom;
        let color = lerp_color(top, bot, t);
        for px in x..x_end {
            img.put_pixel(px, y + dy, color);
        }
    }
}

// ---------------------------------------------------------------------------
// Box / divider helpers
// ---------------------------------------------------------------------------

fn make_divider_bytes(width: u32) -> Vec<u8> {
    let w = width as usize;
    let half = (w.saturating_sub(1)) / 2;
    let mut out = vec![BOX_HZ; half];
    out.push(DIAMOND);
    out.resize(w, BOX_HZ);
    out
}

fn make_ornament_line(width: u32) -> Vec<u8> {
    let w = width as usize;
    let center = [BOX_RT, b' ', DIAMOND, b' ', BOX_LT];
    let start = (w.saturating_sub(center.len())) / 2;
    let mut out = vec![BOX_DB; w];
    for (i, &ch) in center.iter().enumerate() {
        if start + i < w {
            out[start + i] = ch;
        }
    }
    out
}

fn make_double_divider(width: u32) -> Vec<u8> {
    let w = width as usize;
    let half = (w.saturating_sub(1)) / 2;
    let mut out = vec![BOX_DB; half];
    out.push(DIAMOND);
    out.resize(w, BOX_DB);
    out
}

fn make_box_top(label: &str, width: usize) -> Vec<u8> {
    let mut out = vec![b' ', b' ', BOX_TL, BOX_DB, BOX_DB, b' '];
    out.extend_from_slice(label.as_bytes());
    out.push(b' ');
    while out.len() < width - 1 {
        out.push(BOX_DB);
    }
    out.push(BOX_TR);
    out
}

fn make_box_mid(content: &str, width: usize) -> Vec<u8> {
    let mut out = vec![b' ', b' ', BOX_VT, b' ', b' ', BULLET, b' '];
    out.extend_from_slice(content.as_bytes());
    while out.len() < width - 1 {
        out.push(b' ');
    }
    out.push(BOX_VT);
    out
}

fn make_box_bot(label: &str, width: usize) -> Vec<u8> {
    let mut out = vec![b' ', b' ', BOX_BL, BOX_DB, BOX_DB, b' '];
    out.extend_from_slice(label.as_bytes());
    out.push(b' ');
    while out.len() < width - 1 {
        out.push(BOX_DB);
    }
    out.push(BOX_BR);
    out
}

fn day_of_year() -> u32 {
    let mut t: libc::time_t = 0;
    // SAFETY: `t` is a writable time_t.
    unsafe { libc::time(&raw mut t) };
    // SAFETY: `t` is initialized; localtime takes a const pointer.
    let tm = unsafe { libc::localtime(&raw const t) };
    if tm.is_null() {
        return 0;
    }
    #[allow(clippy::cast_sign_loss)]
    // SAFETY: tm is non-null (just checked); points to thread-local static
    // storage valid until the next localtime call on this thread.
    unsafe {
        (*tm).tm_yday as u32
    }
}

// ---------------------------------------------------------------------------
// Big letter bitmap font (A-Z, 7 rows × 3-5 wide)
// ---------------------------------------------------------------------------

/// Returns (width, bitmaps) for A-Z block letters.
/// Each row is a u8 with set bits from MSB (bit 7 = leftmost column).
fn big_letter(ch: u8) -> Option<(u32, [u8; 7])> {
    match ch.to_ascii_uppercase() {
        b'A' => Some((5, [0x70, 0x88, 0x88, 0xF8, 0x88, 0x88, 0x88])),
        b'B' => Some((5, [0xF0, 0x88, 0x88, 0xF0, 0x88, 0x88, 0xF0])),
        b'C' => Some((5, [0x70, 0x88, 0x80, 0x80, 0x80, 0x88, 0x70])),
        b'D' => Some((5, [0xF0, 0x88, 0x88, 0x88, 0x88, 0x88, 0xF0])),
        b'E' => Some((4, [0xF0, 0x80, 0x80, 0xE0, 0x80, 0x80, 0xF0])),
        b'F' => Some((4, [0xF0, 0x80, 0x80, 0xE0, 0x80, 0x80, 0x80])),
        b'G' => Some((5, [0x70, 0x88, 0x80, 0x98, 0x88, 0x88, 0x70])),
        b'H' => Some((5, [0x88, 0x88, 0x88, 0xF8, 0x88, 0x88, 0x88])),
        b'I' => Some((3, [0xE0, 0x40, 0x40, 0x40, 0x40, 0x40, 0xE0])),
        b'J' => Some((4, [0x70, 0x10, 0x10, 0x10, 0x10, 0x90, 0x60])),
        b'K' => Some((5, [0x88, 0x90, 0xA0, 0xC0, 0xA0, 0x90, 0x88])),
        b'L' => Some((4, [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0xF0])),
        b'M' => Some((5, [0x88, 0xD8, 0xA8, 0xA8, 0x88, 0x88, 0x88])),
        b'N' => Some((5, [0x88, 0xC8, 0xA8, 0xA8, 0xA8, 0x98, 0x88])),
        b'O' => Some((5, [0x70, 0x88, 0x88, 0x88, 0x88, 0x88, 0x70])),
        b'P' => Some((5, [0xF0, 0x88, 0x88, 0xF0, 0x80, 0x80, 0x80])),
        b'Q' => Some((5, [0x70, 0x88, 0x88, 0x88, 0xA8, 0x90, 0x68])),
        b'R' => Some((5, [0xF0, 0x88, 0x88, 0xF0, 0xA0, 0x90, 0x88])),
        b'S' => Some((5, [0x70, 0x88, 0x80, 0x70, 0x08, 0x88, 0x70])),
        b'T' => Some((5, [0xF8, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20])),
        b'U' => Some((5, [0x88, 0x88, 0x88, 0x88, 0x88, 0x88, 0x70])),
        b'V' => Some((5, [0x88, 0x88, 0x88, 0x88, 0x50, 0x50, 0x20])),
        b'W' => Some((5, [0x88, 0x88, 0x88, 0x88, 0xA8, 0xD8, 0x88])),
        b'X' => Some((5, [0x88, 0x88, 0x50, 0x20, 0x50, 0x88, 0x88])),
        b'Y' => Some((5, [0x88, 0x88, 0x50, 0x20, 0x20, 0x20, 0x20])),
        b'Z' => Some((5, [0xF8, 0x08, 0x10, 0x20, 0x40, 0x80, 0xF8])),
        _ => None,
    }
}

/// True if pixel (px, py) is set in a big letter's bitmap rows.
fn pixel_set(rows: [u8; 7], px: u32, py: u32) -> bool {
    py < 7 && px < 8 && (rows[py as usize] & (0x80 >> px)) != 0
}

// ---------------------------------------------------------------------------
// 3D block letter renderer (pixel-level)
// ---------------------------------------------------------------------------

struct BigLetter {
    width: u32,
    rows: [u8; 7],
}

#[allow(clippy::cast_precision_loss)]
fn draw_big_title(img: &mut RgbaImage, start_row: u32, text: &str, pal: &Palette, scale: u32) {
    let letters: Vec<BigLetter> = text
        .bytes()
        .filter_map(|ch| big_letter(ch).map(|(w, rows)| BigLetter { width: w, rows }))
        .collect();
    if letters.is_empty() {
        return;
    }

    // Each bitmap pixel = cell_w × cell_h screen pixels
    let cell_w = 2 * GLYPH_W * scale;
    let cell_h = GLYPH_H * scale;

    // Total width in bitmap pixels (1-pixel gap between letters)
    #[allow(clippy::cast_possible_truncation)]
    let total_cells: u32 =
        letters.iter().map(|l| l.width).sum::<u32>() + (letters.len() as u32 - 1);
    let total_w = total_cells * cell_w;
    let img_w = COLS * GLYPH_W * scale;
    let start_x = (img_w.saturating_sub(total_w)) / 2;
    let start_y = start_row * GLYPH_H * scale;

    // 3D extrusion: 4 layers, each offset by (ex, ey) pixels
    let depth: u32 = 4;
    let ex = scale * 3;
    let ey = scale * 3;
    let bevel = (scale * 2).max(1);

    // Pre-compute extrusion layer colors (deepest=darkest → shallowest=brighter)
    let extrude_mid = scale_color(pal.title_face, 0.35);

    // Pass 1: Extrusion layers (deepest first, so shallower overwrites)
    for d in (1..=depth).rev() {
        let t = d as f32 / depth as f32;
        let color = lerp_color(extrude_mid, pal.title_shadow, t);
        let mut cursor = 0u32;
        for letter in &letters {
            for by in 0..7u32 {
                for bx in 0..letter.width {
                    if pixel_set(letter.rows, bx, by) {
                        let x = start_x + (cursor + bx) * cell_w + d * ex;
                        let y = start_y + by * cell_h + d * ey;
                        fill_rect(img, x, y, cell_w, cell_h, color);
                    }
                }
            }
            cursor += letter.width + 1;
        }
    }

    // Pass 2: Face with vertical gradient + bevel edges
    let face_top = scale_color(pal.title_face, 1.2);
    let face_bot = scale_color(pal.title_face, 0.7);
    let bevel_dark = scale_color(pal.title_face, 0.3);

    let mut cursor = 0u32;
    for letter in &letters {
        for by in 0..7u32 {
            for bx in 0..letter.width {
                if !pixel_set(letter.rows, bx, by) {
                    continue;
                }
                let x = start_x + (cursor + bx) * cell_w;
                let y = start_y + by * cell_h;

                // Gradient face
                fill_rect_vgradient(img, x, y, cell_w, cell_h, face_top, face_bot);

                // Edge detection
                let has_top = by == 0 || !pixel_set(letter.rows, bx, by - 1);
                let has_left = bx == 0 || !pixel_set(letter.rows, bx - 1, by);
                let has_bottom = by >= 6 || !pixel_set(letter.rows, bx, by + 1);
                let has_right = bx + 1 >= letter.width || !pixel_set(letter.rows, bx + 1, by);

                // Dark bevels (bottom/right) drawn first
                if has_bottom {
                    fill_rect(img, x, y + cell_h - bevel, cell_w, bevel, bevel_dark);
                }
                if has_right {
                    fill_rect(img, x + cell_w - bevel, y, bevel, cell_h, bevel_dark);
                }
                // Bright bevels (top/left) overwrite corners
                if has_top {
                    fill_rect(img, x, y, cell_w, bevel, pal.title_hi);
                }
                if has_left {
                    fill_rect(img, x, y, bevel, cell_h, pal.title_hi);
                }
            }
        }
        cursor += letter.width + 1;
    }
}

// ---------------------------------------------------------------------------
// Shared content helpers
// ---------------------------------------------------------------------------

fn draw_system_info(
    img: &mut RgbaImage,
    start_row: u32,
    info: &SystemInfo,
    pal: &Palette,
    scale: u32,
) {
    let info_lines: [(&str, String); 6] = [
        ("0P3R4T1NG", info.os.to_uppercase()),
        ("4RCH1T3CT", info.arch.to_uppercase()),
        ("H0STN4M3 ", info.hostname.to_uppercase()),
        ("D4T3     ", info.date.clone()),
        ("L04D     ", info.load_string()),
        ("M3M0RY   ", info.memory_string()),
    ];
    for (i, (label, value)) in info_lines.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let row = start_row + i as u32;
        let mut line: Vec<u8> = vec![b' ', b' ', BULLET, b' '];
        line.extend_from_slice(label.as_bytes());
        line.extend_from_slice(b"  ");
        #[allow(clippy::cast_possible_truncation)]
        let val_col = 2 + line.len() as u32;
        draw_bytes(img, 2, row, &line, pal.label_fg, pal.bg, scale);

        let val_text = format!("[ {value} ]");
        draw_text(img, val_col, row, &val_text, pal.value_fg, pal.bg, scale);
    }
}

fn draw_tagline_box(
    img: &mut RgbaImage,
    start_row: u32,
    info: &SystemInfo,
    pal: &Palette,
    scale: u32,
) {
    let taglines = [
        "proudly serving the scene since 1993",
        "where the elstrEEt meet",
        "another fine release from the underground",
        "cracked by the best, spread by the rest",
        "the future is now, old man",
        "10 nodes / USR Courier V.Everything",
        "greets to all groups worldwide",
        "the underground never sleeps",
    ];
    let tag_idx = day_of_year() as usize % taglines.len();
    let tagline = taglines[tag_idx];

    let top_label = "Terminal Underground Division";
    let node_label = format!("NODE: {}", info.hostname.to_uppercase());
    // Box width = max of all three rows (prefix + content + suffix)
    let box_w = (6 + top_label.len() + 4)
        .max(7 + tagline.len() + 2)
        .max(6 + node_label.len() + 4);
    let box_top = make_box_top(top_label, box_w);
    let box_mid = make_box_mid(tagline, box_w);
    let box_bot = make_box_bot(&node_label, box_w);
    draw_bytes(img, 0, start_row, &box_top, pal.border_fg, pal.bg, scale);
    draw_bytes(img, 0, start_row + 1, &box_mid, pal.label_fg, pal.bg, scale);
    draw_bytes(
        img,
        0,
        start_row + 2,
        &box_bot,
        pal.border_fg,
        pal.bg,
        scale,
    );
}

// ---------------------------------------------------------------------------
// Dashboard pixel-level rendering (btop-inspired)
// ---------------------------------------------------------------------------

#[allow(dead_code, clippy::cast_precision_loss, clippy::many_single_char_names)]
fn fill_rect_hgradient(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    left: Rgba<u8>,
    right: Rgba<u8>,
) {
    if w == 0 {
        return;
    }
    let x_end = (x + w).min(img.width());
    let y_end = (y + h).min(img.height());
    let denom = (w - 1).max(1) as f32;
    for dx in 0..w {
        if x + dx >= x_end {
            break;
        }
        let t = dx as f32 / denom;
        let color = lerp_color(left, right, t);
        for py in y..y_end {
            img.put_pixel(x + dx, py, color);
        }
    }
}

fn threshold_color(pct: f32, pal: &Palette) -> Rgba<u8> {
    if pct > 0.9 {
        pal.bar_crit
    } else if pct > 0.7 {
        pal.bar_warn
    } else {
        pal.value_fg
    }
}

/// Draw a 3D beveled progress bar at pixel coordinates.
#[allow(
    dead_code,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]
fn draw_bar_3d(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    bar_w: u32,
    bar_h: u32,
    pct: f32,
    color: Rgba<u8>,
    pal: &Palette,
) {
    let pct = pct.clamp(0.0, 1.0);
    let bevel = 2u32;

    // Dark inset background
    let inset = scale_color(pal.bg, 1.8);
    fill_rect(img, x, y, bar_w, bar_h, inset);

    // Dark border around the bar area
    let border = scale_color(pal.bg, 2.5);
    fill_rect(img, x, y, bar_w, 1, border);
    fill_rect(img, x, y + bar_h - 1, bar_w, 1, border);
    fill_rect(img, x, y, 1, bar_h, border);
    fill_rect(img, x + bar_w - 1, y, 1, bar_h, border);

    // Filled portion
    let fill_w = (pct * (bar_w - 2) as f32) as u32;
    if fill_w > 0 {
        let brighter = scale_color(color, 1.3);
        fill_rect_hgradient(img, x + 1, y + 1, fill_w, bar_h - 2, color, brighter);

        // Top bevel (bright)
        let hi = scale_color(color, 1.5);
        fill_rect(img, x + 1, y + 1, fill_w, bevel.min(bar_h - 2), hi);

        // Bottom bevel (dark)
        let lo = scale_color(color, 0.5);
        let bot_y = y + bar_h - 1 - bevel.min(bar_h - 2);
        fill_rect(img, x + 1, bot_y, fill_w, bevel.min(bar_h - 2), lo);
    }
}

/// Draw a stacked bar with multiple colored segments, 3D bevel treatment.
#[allow(
    dead_code,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn draw_stacked_bar_3d(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    bar_w: u32,
    bar_h: u32,
    segments: &[(f32, Rgba<u8>)],
    pal: &Palette,
) {
    let bevel = 2u32;
    let inner_w = bar_w.saturating_sub(2);
    let inner_h = bar_h.saturating_sub(2);

    // Dark inset background
    let inset = scale_color(pal.bg, 1.8);
    fill_rect(img, x, y, bar_w, bar_h, inset);

    // Dark border
    let border = scale_color(pal.bg, 2.5);
    fill_rect(img, x, y, bar_w, 1, border);
    fill_rect(img, x, y + bar_h - 1, bar_w, 1, border);
    fill_rect(img, x, y, 1, bar_h, border);
    fill_rect(img, x + bar_w - 1, y, 1, bar_h, border);

    // Draw each segment
    let mut cursor = 0u32;
    for &(pct, color) in segments {
        let seg_w = (pct.clamp(0.0, 1.0) * inner_w as f32) as u32;
        if seg_w == 0 {
            continue;
        }
        let sx = x + 1 + cursor;
        let remaining = inner_w.saturating_sub(cursor);
        let actual_w = seg_w.min(remaining);
        if actual_w == 0 {
            break;
        }

        let brighter = scale_color(color, 1.2);
        fill_rect_hgradient(img, sx, y + 1, actual_w, inner_h, color, brighter);

        // Top bevel
        let hi = scale_color(color, 1.5);
        fill_rect(img, sx, y + 1, actual_w, bevel.min(inner_h), hi);

        // Bottom bevel
        let lo = scale_color(color, 0.5);
        let bot_y = y + 1 + inner_h.saturating_sub(bevel.min(inner_h));
        fill_rect(img, sx, bot_y, actual_w, bevel.min(inner_h), lo);

        cursor += actual_w;
    }
}

/// Draw 3 side-by-side mini vertical bars for load trend.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]
fn draw_mini_bars(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    values: &[f32; 3],
    max: f32,
    bar_w: u32,
    bar_h: u32,
    pal: &Palette,
) {
    let gap = 2u32;
    let max = max.max(0.01);
    let inset = scale_color(pal.bg, 1.8);
    let border = scale_color(pal.bg, 2.5);

    for (i, &val) in values.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let bx = x + i as u32 * (bar_w + gap);
        let pct = (val / max).clamp(0.0, 1.0);
        let fill_h = (pct * (bar_h - 2) as f32) as u32;
        let color = threshold_color(pct, pal);

        // Background
        fill_rect(img, bx, y, bar_w, bar_h, inset);
        // Border
        fill_rect(img, bx, y, bar_w, 1, border);
        fill_rect(img, bx, y + bar_h - 1, bar_w, 1, border);
        fill_rect(img, bx, y, 1, bar_h, border);
        fill_rect(img, bx + bar_w - 1, y, 1, bar_h, border);

        // Fill from bottom
        if fill_h > 0 {
            let fy = y + 1 + (bar_h - 2 - fill_h);
            let hi = scale_color(color, 1.4);
            fill_rect_vgradient(img, bx + 1, fy, bar_w - 2, fill_h, hi, color);
        }
    }
}

/// Draw a text label at pixel coordinates using the CP437 glyph font.
#[allow(clippy::cast_possible_truncation)]
fn draw_text_px(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    text: &str,
    fg: Rgba<u8>,
    bg_color: Rgba<u8>,
    scale: u32,
) {
    let gw = GLYPH_W * scale;
    for (i, ch) in text.bytes().enumerate() {
        let cx = x + i as u32 * gw;
        for py in 0..GLYPH_H {
            for px in 0..GLYPH_W {
                let color = if font::glyph_pixel(ch, px, py) {
                    fg
                } else {
                    bg_color
                };
                for sy in 0..scale {
                    for sx in 0..scale {
                        let ix = cx + px * scale + sx;
                        let iy = y + py * scale + sy;
                        if ix < img.width() && iy < img.height() {
                            img.put_pixel(ix, iy, color);
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn draw_text_px_transparent(
    img: &mut RgbaImage,
    x: u32,
    y: u32,
    text: &str,
    fg: Rgba<u8>,
    scale: u32,
) {
    let gw = GLYPH_W * scale;
    for (i, ch) in text.bytes().enumerate() {
        let cx = x + i as u32 * gw;
        for py in 0..GLYPH_H {
            for px in 0..GLYPH_W {
                if font::glyph_pixel(ch, px, py) {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let ix = cx + px * scale + sx;
                            let iy = y + py * scale + sy;
                            if ix < img.width() && iy < img.height() {
                                img.put_pixel(ix, iy, fg);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[allow(dead_code, clippy::cast_precision_loss)]
fn format_bytes_short(bytes: u64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb >= 100.0 {
        format!("{gb:.0}")
    } else if gb >= 10.0 {
        format!("{gb:.1}")
    } else {
        format!("{gb:.2}")
    }
}

fn format_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    if days > 0 {
        format!("{days}d {hours}h")
    } else {
        let mins = (secs % 3600) / 60;
        format!("{hours}h {mins}m")
    }
}

// ---------------------------------------------------------------------------
// Radial arc gauge rendering
// ---------------------------------------------------------------------------

/// Draw an anti-aliased thick arc with optional 3D bevel effect.
///
/// Arc is a bottom semicircle (∪ cup shape, like a speedometer).
/// Covers angles in `[angle_from, angle_to]` where angles are measured
/// with `atan2(dy, dx)` — positive angles map to pixels below `cy`.
/// `angle_from=0, angle_to=π` gives a full bottom semicircle.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]
fn draw_arc(
    img: &mut RgbaImage,
    cx: f32,
    cy: f32,
    r_outer: f32,
    r_inner: f32,
    angle_from: f32,
    angle_to: f32,
    color: Rgba<u8>,
    bevel: bool,
) {
    let x_min = (cx - r_outer - 1.0).max(0.0) as u32;
    let x_max = ((cx + r_outer + 1.0) as u32).min(img.width().saturating_sub(1));
    // Only need tiny region above cy (AA), arc extends below
    let y_min = (cy - 1.5).max(0.0) as u32;
    let y_max = ((cy + r_outer + 1.0) as u32).min(img.height().saturating_sub(1));
    let half_px = (1.0 / r_outer).max(0.01);

    for py in y_min..=y_max {
        for px in x_min..=x_max {
            let dx = px as f32 - cx;
            let dy = py as f32 - cy;
            let dist = dx.hypot(dy);

            if dist > r_outer + 1.0 || dist < r_inner - 1.0 {
                continue;
            }

            // atan2(dy, dx): positive angles = below cy = ∪ cup shape
            let angle = dy.atan2(dx);

            // Radial coverage (smooth at inner/outer edges)
            let radial = smoothstep(r_outer + 0.5, r_outer - 0.5, dist)
                * smoothstep(r_inner - 0.5, r_inner + 0.5, dist);

            // Angular coverage: inside [angle_from, angle_to]
            let angular = smoothstep(angle_from - half_px, angle_from + half_px, angle)
                * smoothstep(angle_to + half_px, angle_to - half_px, angle);

            let coverage = radial * angular;
            if coverage > 0.001 {
                let final_color = if bevel && r_outer > r_inner {
                    let radial_t = ((dist - r_inner) / (r_outer - r_inner)).clamp(0.0, 1.0);
                    let bevel_factor = radial_t.mul_add(0.7, 0.6);
                    scale_color(color, bevel_factor)
                } else {
                    color
                };
                blend_pixel(img, px, py, final_color, coverage);
            }
        }
    }
}

/// Draw a complete radial arc gauge (∪ cup/speedometer shape):
/// background track, colored fill, value text inside, label below.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_arguments
)]
fn draw_arc_gauge(
    img: &mut RgbaImage,
    cx: f32,
    cy: f32,
    r_outer: f32,
    r_inner: f32,
    pct: f32,
    pal: &Palette,
    label: &str,
    scale: u32,
) {
    let pi = std::f32::consts::PI;

    // Dark inset background track (full bottom semicircle: 0 to π)
    let bg_inset = scale_color(pal.bg, 3.5);
    draw_arc(img, cx, cy, r_outer, r_inner, 0.0, pi, bg_inset, false);

    // Filled arc: sweeps from left (π) toward right (0) based on pct.
    // In our coordinate system (atan2(dy,dx)), left=π, right=0.
    // Fill starts at π and sweeps down to π*(1-pct).
    if pct > 0.005 {
        let fill_start = pi * (1.0 - pct);
        let fill_color = threshold_color(pct, pal);
        draw_arc(
            img,
            cx,
            cy,
            r_outer - 1.0,
            r_inner + 1.0,
            fill_start,
            pi,
            fill_color,
            true,
        );
    }

    // Value text centered inside the cup
    let gw = GLYPH_W * scale;
    let gh = GLYPH_H * scale;

    let pct_text = format!("{:.0}%", pct * 100.0);
    let text_w = pct_text.len() as u32 * gw;
    let text_x = (cx as u32).saturating_sub(text_w / 2);
    // Place text fully inside the inner cup: top at ~cy+scale, bottom well above r_inner
    let text_y = (cy + r_inner * 0.15) as u32;
    draw_text_px_transparent(img, text_x, text_y, &pct_text, pal.value_fg, scale);

    // Label below the arc bottom
    let label_w = label.len() as u32 * gw;
    let label_x = (cx as u32).saturating_sub(label_w / 2);
    let label_y = (cy + r_outer) as u32 + scale;
    draw_text_px_transparent(img, label_x, label_y, label, pal.label_fg, scale);
    let _ = gh;
}

/// Render 3 radial arc gauges side by side for CPU, MEM, DSK.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn draw_gauge_panel(
    img: &mut RgbaImage,
    start_row: u32,
    info: &SystemInfo,
    pal: &Palette,
    scale: u32,
) {
    let row_h = GLYPH_H * scale;
    let gh = GLYPH_H * scale;
    let img_w = COLS * GLYPH_W * scale;

    // 4 text rows of gauge area = 128px at scale 2
    // Arc hangs down from cy; need room for arc + label text below
    let r_outer = (row_h * 2) as f32; // 64px at scale 2
    let r_inner = r_outer - (10 * scale) as f32; // 44px, 20px thick arc

    // cy at the top of the gauge area (arc hangs below)
    let area_top = (start_row * row_h) as f32;
    let cy = area_top + (2 * scale) as f32;

    // Three evenly spaced gauge centers
    let spacing = img_w as f32 / 3.0;
    let centers = [spacing * 0.5, spacing * 1.5, spacing * 2.5];
    let _ = gh; // suppress unused warning

    // CPU: load_1 / ncores
    let cpu_pct = (info.load_1 as f32 / info.ncores.max(1) as f32).clamp(0.0, 1.0);
    draw_arc_gauge(
        img, centers[0], cy, r_outer, r_inner, cpu_pct, pal, "CPU", scale,
    );

    // MEM: used / total
    let mem_pct = if info.mem_total > 0 {
        (info.mem_used as f32 / info.mem_total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    draw_arc_gauge(
        img, centers[1], cy, r_outer, r_inner, mem_pct, pal, "MEM", scale,
    );

    // DSK: used / total
    let dsk_pct = if info.disk_total > 0 {
        (info.disk_used as f32 / info.disk_total as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    draw_arc_gauge(
        img, centers[2], cy, r_outer, r_inner, dsk_pct, pal, "DSK", scale,
    );
}

/// btop-inspired dashboard replacing `draw_system_info` for block3d.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
fn draw_dashboard(
    img: &mut RgbaImage,
    start_row: u32,
    info: &SystemInfo,
    pal: &Palette,
    scale: u32,
) {
    let gw = GLYPH_W * scale;
    let row_h = GLYPH_H * scale;
    let label_col = 2u32;
    let bar_x = (label_col + 5) * gw;
    let bar_h = row_h - 4 * scale;
    let bar_y_off = 2 * scale;

    // Rows 0-3: Radial arc gauges (CPU, MEM, DSK)
    draw_gauge_panel(img, start_row, info, pal, scale);

    // Row 4: Load trend mini-bars + values
    {
        let row = start_row + 4;
        let row_y = row * row_h;
        draw_text(img, label_col, row, "LOD", pal.label_fg, pal.bg, scale);

        let ncores = info.ncores.max(1) as f32;
        let values = [info.load_1 as f32, info.load_5 as f32, info.load_15 as f32];
        let mini_w = 8 * scale;
        let mini_h = bar_h;
        draw_mini_bars(
            img,
            bar_x,
            row_y + bar_y_off,
            &values,
            ncores,
            mini_w,
            mini_h,
            pal,
        );

        // Load values as text after the mini bars
        let text_x = bar_x + 3 * (mini_w + 2) + gw;
        let load_text = format!(
            "{:.2}  {:.2}  {:.2}",
            info.load_1, info.load_5, info.load_15
        );
        draw_text_px(
            img,
            text_x,
            row_y + bar_y_off,
            &load_text,
            pal.value_fg,
            pal.bg,
            scale,
        );
    }

    // Row 5: Uptime, process count, IP
    {
        let row = start_row + 5;
        let uptime = format_uptime(info.uptime_secs);
        let ip = if info.ip_addr.is_empty() {
            "N/A"
        } else {
            &info.ip_addr
        };
        let line = format!("UPT {uptime}  PRC {}  IP {ip}", info.proc_count);
        draw_text(img, label_col, row, &line, pal.value_fg, pal.bg, scale);
    }

    // Row 6: hostname decorative line
    {
        let row = start_row + 6;
        let host_line = format!(
            "{} {} / {} / {}",
            DIAMOND as char,
            info.hostname.to_uppercase(),
            info.os.to_uppercase(),
            info.arch.to_uppercase()
        );
        let mut line_bytes: Vec<u8> = vec![b' ', b' ', BOX_HZ, b' '];
        line_bytes.extend_from_slice(host_line.as_bytes());
        line_bytes.push(b' ');
        while line_bytes.len() < COLS as usize {
            line_bytes.push(BOX_HZ);
        }
        draw_bytes(img, 0, row, &line_bytes, pal.border_fg, pal.bg, scale);
    }
}

// ---------------------------------------------------------------------------
// Banner layouts
// ---------------------------------------------------------------------------

fn draw_classic(img: &mut RgbaImage, pal: &Palette, scale: u32) {
    let info = SystemInfo::gather();

    // Row 1: gradient bar ░▒▓█
    draw_gradient_bar(img, 1, &pal.gradient, false, scale);

    // Rows 3-6: "TERMINAL UNDERGROUND" header with shadow
    let title1 = "TERMINAL";
    let title2 = "UNDERGROUND";
    #[allow(clippy::cast_possible_truncation)]
    let t1_col = (COLS - title1.len() as u32) / 2;
    #[allow(clippy::cast_possible_truncation)]
    let t2_col = (COLS - title2.len() as u32) / 2;
    draw_block_text(img, t1_col, 3, title1, pal.header_fg, pal.bg, scale);
    draw_block_text(img, t2_col, 5, title2, pal.header_fg, pal.bg, scale);

    // Row 8: divider ────◆────
    let div = make_divider_bytes(COLS);
    draw_bytes(img, 0, 8, &div, pal.border_fg, pal.bg, scale);

    // Rows 10-15: system info
    draw_system_info(img, 10, &info, pal, scale);

    // Rows 17-19: tagline box
    draw_tagline_box(img, 17, &info, pal, scale);

    // Row 23: reverse gradient bar █▓▒░
    draw_gradient_bar(img, 23, &pal.gradient, true, scale);
}

/// Returns the total number of rows used.
fn draw_block3d(img: &mut RgbaImage, pal: &Palette, scale: u32, title: Option<&str>) -> u32 {
    let info = SystemInfo::gather();

    // Row 1: gradient bar ░▒▓█
    draw_gradient_bar(img, 1, &pal.gradient, false, scale);

    // Row 2: ornament ═══╡ ◆ ╞═══
    let ornament = make_ornament_line(COLS);
    draw_bytes(img, 0, 2, &ornament, pal.border_fg, pal.bg, scale);

    let dash_row = if let Some(t) = title {
        // Rows 3-9: big 3D block letters
        draw_big_title(img, 3, t, pal, scale);

        // Row 11: double divider ═══◆═══
        let div = make_double_divider(COLS);
        draw_bytes(img, 0, 11, &div, pal.border_fg, pal.bg, scale);
        12
    } else {
        // No title — divider then dashboard starts higher
        let div = make_double_divider(COLS);
        draw_bytes(img, 0, 3, &div, pal.border_fg, pal.bg, scale);
        4
    };

    // Dashboard (btop-inspired)
    draw_dashboard(img, dash_row, &info, pal, scale);

    // Tagline box
    let tagline_row = dash_row + 7;
    draw_tagline_box(img, tagline_row, &info, pal, scale);

    // Reverse gradient bar right after content
    let bottom_row = tagline_row + 4;
    draw_gradient_bar(img, bottom_row, &pal.gradient, true, scale);

    bottom_row + 2 // total rows used (bar + 1 row padding)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Base64 encoder (no external dep)
// ---------------------------------------------------------------------------

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = u32::from(b0) << 16 | u32::from(b1) << 8 | u32::from(b2);
        out.push(B64_CHARS[(n >> 18 & 0x3F) as usize] as char);
        out.push(B64_CHARS[(n >> 12 & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_CHARS[(n >> 6 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_CHARS[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Kitty graphics protocol output
// ---------------------------------------------------------------------------

/// Write PNG data as Kitty graphics protocol escape sequences.
fn write_kitty_graphics(png_data: &[u8], out: &mut impl Write) {
    const CHUNK_SIZE: usize = 4096;

    let encoded = base64_encode(png_data);
    let bytes = encoded.as_bytes();
    let chunks: Vec<&[u8]> = bytes.chunks(CHUNK_SIZE).collect();
    let last = chunks.len().saturating_sub(1);

    for (i, chunk) in chunks.iter().enumerate() {
        let m = u8::from(i < last);
        if i == 0 {
            write!(out, "\x1b_Gf=100,a=T,q=2,m={m};").expect("write failed");
        } else {
            write!(out, "\x1b_Gm={m};").expect("write failed");
        }
        out.write_all(chunk).expect("write failed");
        out.write_all(b"\x1b\\").expect("write failed");
    }
    // Newline after image so the shell prompt starts on a fresh line
    out.write_all(b"\n").expect("write failed");
}

/// Pipe PNG through chafa for tmux or terminals without Kitty support.
fn write_via_chafa(png_data: &[u8]) {
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new("chafa")
        .args(["--size=80x25", "-"])
        .stdin(Stdio::piped())
        .spawn()
    else {
        // chafa not available, write raw PNG as last resort
        let mut out = io::stdout().lock();
        out.write_all(png_data).ok();
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(png_data).ok();
    }
    child.wait().ok();
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Generates the banner and writes it to stdout.
///
/// Output modes:
/// - Direct terminal: Kitty graphics protocol (pixel-perfect)
/// - Inside tmux: pipes through chafa (tmux can't manage Kitty graphics layer)
/// - `png` / `classic-png`: raw PNG bytes (for saving to files)
pub fn generate(scale: u32, palette_name: &str, banner_type: Option<&str>, title: Option<&str>) {
    let pal = palette_by_name(palette_name);
    let width = COLS * GLYPH_W * scale;

    let (resolved, raw_png) = match banner_type {
        Some("png") => ("block3d", true),
        Some("classic-png") => ("classic", true),
        Some(other) => (other, false),
        None => ("block3d", false),
    };

    // Allocate full canvas, render, then crop to actual content height.
    let max_height = ROWS * GLYPH_H * scale;
    let mut img: RgbaImage = ImageBuffer::from_pixel(width, max_height, pal.bg);

    let rows_used = match resolved {
        "classic" => {
            draw_classic(&mut img, &pal, scale);
            ROWS
        }
        _ => draw_block3d(&mut img, &pal, scale, title),
    };

    let height = (rows_used * GLYPH_H * scale).min(max_height);
    let cropped = image::imageops::crop_imm(&img, 0, 0, width, height).to_image();

    // Encode to PNG in memory
    let mut png_buf: Vec<u8> = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_buf);
    encoder
        .write_image(
            cropped.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )
        .expect("failed to encode PNG");

    if raw_png {
        let mut out = io::BufWriter::new(io::stdout().lock());
        out.write_all(&png_buf).expect("failed to write PNG");
        out.flush().expect("failed to flush stdout");
    } else {
        let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
        if in_tmux {
            write_via_chafa(&png_buf);
        } else {
            let mut out = io::BufWriter::new(io::stdout().lock());
            write_kitty_graphics(&png_buf, &mut out);
            out.flush().expect("failed to flush stdout");
        }
    }
}
