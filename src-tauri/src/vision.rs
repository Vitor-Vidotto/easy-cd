use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits,
    GetWindowDC, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetDesktopWindow, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN,
};

/// Ordem exata na barra do Albion Online (da esquerda para a direita):
/// E (Ultimate) -> R (Peito) -> D (Elmo) -> F (Bota) -> 1 (Poção)
pub const SLOTS: [(&str, &str, &str); 5] = [
    ("E", "ultimate", "Ult (E)"),
    ("R", "peito", "Peito (R)"),
    ("D", "elmo", "Elmo (D)"),
    ("F", "bota", "Bota (F)"),
    ("1", "pocao", "Poção (1)"),
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RegionBBox {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotPreview {
    pub key: String,
    pub field: String,
    pub label: String,
    pub active: bool,
    pub seconds: Option<u32>,
    pub image_data_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionStateSnapshot {
    pub bbox: Option<RegionBBox>,
    pub is_tracking: bool,
    pub slots: Vec<SlotPreview>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CooldownChangePayload {
    pub changes: HashMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedConfig {
    bbox: Option<RegionBBox>,
}

pub struct SharedVisionState {
    pub bbox: Option<RegionBBox>,
    pub is_tracking: bool,
    pub is_overlay_locked: bool,
    pub slot_active: [bool; 5],
    pub hit_streak: [u8; 5],
    pub miss_streak: [u8; 5],
    pub low_jump_streak: [u8; 5],
    pub last_read_sec: [u32; 5],
    pub last_hit_at: [Instant; 5],
    pub config_path: PathBuf,
}

impl SharedVisionState {
    pub fn new(config_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&config_dir);
        let config_path = config_dir.join("vision_config.json");
        let bbox = fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str::<PersistedConfig>(&s).ok())
            .and_then(|c| c.bbox);

        Self {
            bbox,
            is_tracking: false,
            is_overlay_locked: false,
            slot_active: [false; 5],
            hit_streak: [0; 5],
            miss_streak: [0; 5],
            low_jump_streak: [0; 5],
            last_read_sec: [0; 5],
            last_hit_at: [Instant::now(); 5],
            config_path,
        }
    }

    pub fn save_bbox(&mut self, bbox: Option<RegionBBox>) {
        self.bbox = bbox;
        let persisted = PersistedConfig { bbox };
        if let Ok(json) = serde_json::to_string_pretty(&persisted) {
            let _ = fs::write(&self.config_path, json);
        }
    }

    pub fn reset_streaks(&mut self) {
        self.slot_active = [false; 5];
        self.hit_streak = [0; 5];
        self.miss_streak = [0; 5];
        self.low_jump_streak = [0; 5];
        self.last_read_sec = [0; 5];
        self.last_hit_at = [Instant::now(); 5];
    }
}

pub type VisionStateHandle = Arc<Mutex<SharedVisionState>>;

pub fn init_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn get_virtual_screen_bounds() -> (i32, i32, i32, i32) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if vw > 0 && vh > 0 {
            (vx, vy, vw, vh)
        } else {
            (0, 0, 1920, 1080)
        }
    }
}

/// Captura apenas o retângulo físico `(x1, y1, x2, y2)` em BGRA8 top-down via Win32 BitBlt.
/// Sem `CAPTUREBLT`, janelas transparentes (como o próprio overlay do Tauri) são ignoradas!
pub fn grab_screen_region_bgra(bbox: RegionBBox) -> Option<(usize, usize, Vec<u8>)> {
    let w = bbox.x2 - bbox.x1;
    let h = bbox.y2 - bbox.y1;
    if w <= 8 || h <= 8 {
        return None;
    }

    unsafe {
        let hwnd = GetDesktopWindow();
        let hdc_screen = GetWindowDC(hwnd);
        if hdc_screen.is_invalid() {
            return None;
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            let _ = ReleaseDC(hwnd, hdc_screen);
            return None;
        }

        let hbmp = CreateCompatibleBitmap(hdc_screen, w, h);
        if hbmp.is_invalid() {
            let _ = DeleteDC(hdc_mem);
            let _ = ReleaseDC(hwnd, hdc_screen);
            return None;
        }

        let old_obj = SelectObject(hdc_mem, hbmp);
        let blt_ok = BitBlt(hdc_mem, 0, 0, w, h, hdc_screen, bbox.x1, bbox.y1, SRCCOPY).is_ok();

        let mut out_buf = vec![0u8; (w as usize) * (h as usize) * 4];
        let mut success = false;

        if blt_ok {
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // Top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };

            let lines = GetDIBits(
                hdc_mem,
                hbmp,
                0,
                h as u32,
                Some(out_buf.as_mut_ptr() as *mut c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if lines == h {
                for px in out_buf.chunks_exact_mut(4) {
                    px[3] = 255;
                }
                success = true;
            }
        }

        let _ = SelectObject(hdc_mem, old_obj);
        let _ = DeleteObject(hbmp);
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(hwnd, hdc_screen);

        if success {
            Some((w as usize, h as usize, out_buf))
        } else {
            None
        }
    }
}

/// Divide a barra capturada em 5 slots horizontais iguais (E, R, D, F, 1).
pub fn split_bar_into_slots_bgra(
    bar_w: usize,
    bar_h: usize,
    bar_bgra: &[u8],
) -> Vec<(usize, usize, Vec<u8>)> {
    let slot_w_f = bar_w as f64 / 5.0;
    let mut slots = Vec::with_capacity(5);

    for i in 0..5 {
        let x1 = ((i as f64) * slot_w_f).round() as usize;
        let x2 = (((i + 1) as f64) * slot_w_f).round().min(bar_w as f64) as usize;
        let sw = x2.saturating_sub(x1);
        let mut buf = vec![0u8; sw * bar_h * 4];
        for y in 0..bar_h {
            let src_start = (y * bar_w + x1) * 4;
            let dst_start = y * sw * 4;
            buf[dst_start..dst_start + sw * 4]
                .copy_from_slice(&bar_bgra[src_start..src_start + sw * 4]);
        }
        slots.push((sw, bar_h, buf));
    }
    slots
}

/// Gera miniatura PNG 64x64 em Base64 (`data:image/png;base64,...`) para o preview da interface.
pub fn encode_slot_thumbnail_data_url(sw: usize, sh: usize, bgra: &[u8]) -> String {
    if sw == 0 || sh == 0 {
        return String::new();
    }
    let tw = 64usize;
    let th = 64usize;
    let mut rgba = vec![0u8; tw * th * 4];

    for ty in 0..th {
        let sy = (ty * sh) / th;
        for tx in 0..tw {
            let sx = (tx * sw) / tw;
            let s_idx = (sy * sw + sx) * 4;
            let d_idx = (ty * tw + tx) * 4;
            rgba[d_idx] = bgra[s_idx + 2];     // R
            rgba[d_idx + 1] = bgra[s_idx + 1]; // G
            rgba[d_idx + 2] = bgra[s_idx];     // B
            rgba[d_idx + 3] = 255;             // A
        }
    }

    let mut png_bytes = Vec::with_capacity(4096);
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, tw as u32, th as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        if let Ok(mut writer) = encoder.write_header() {
            let _ = writer.write_image_data(&rgba);
        }
    }
    format!("data:image/png;base64,{}", BASE64.encode(&png_bytes))
}

/// Extrai os dígitos brancos puros com sombra projetada inferior-direita (drop-shadow)
/// do centro do slot e monta o canvas BGRA8 para o Windows OCR.
/// Retorna `(canvas_w, canvas_h, bgra_canvas, num_components)`.
/// Funciona mesmo quando o relógio do cooldown já está quase 100% claro sobre ícones brilhantes!
pub fn prepare_ocr_canvas_bgra(
    slot_w: usize,
    slot_h: usize,
    slot_bgra: &[u8],
) -> Option<(usize, usize, Vec<u8>, usize)> {
    if slot_w < 16 || slot_h < 16 {
        return None;
    }

    // Recorta apenas o centro do círculo (ignora 'x10' no topo, atalho e nicks de jogadores passando embaixo)
    let y0_c = ((slot_h as f32) * 0.25) as usize;
    let y1_c = ((slot_h as f32) * 0.73) as usize;
    let x0_c = ((slot_w as f32) * 0.16) as usize;
    let x1_c = ((slot_w as f32) * 0.84) as usize;

    let cw_c = x1_c.saturating_sub(x0_c);
    let ch_c = y1_c.saturating_sub(y0_c);
    if cw_c < 10 || ch_c < 10 {
        return None;
    }

    let mut val_map = vec![0u8; cw_c * ch_c];
    let mut white_mask = vec![false; cw_c * ch_c];

    for cy in 0..ch_c {
        let sy = y0_c + cy;
        for cx in 0..cw_c {
            let sx = x0_c + cx;
            let idx = (sy * slot_w + sx) * 4;
            let b = slot_bgra[idx];
            let g = slot_bgra[idx + 1];
            let r = slot_bgra[idx + 2];

            let v = r.max(g).max(b);
            let min_rgb = r.min(g).min(b);
            let s = if v == 0 {
                0u8
            } else {
                (((v as u32 - min_rgb as u32) * 255) / (v as u32)) as u8
            };

            let m_idx = cy * cw_c + cx;
            val_map[m_idx] = v;
            // Branco puro dos números de cooldown (separa do azul-claro de mantos e das ondas brancas do elmo)
            if v >= 218 && s <= 29 {
                white_mask[m_idx] = true;
            }
        }
    }

    let mut labels = vec![0u32; cw_c * ch_c];
    let mut current_label = 0u32;
    let mut queue = VecDeque::new();

    struct CompCandidate {
        label: u32,
        cy: f32,
        ch: f32,
        dist_center: f32,
        min_v: u8,
        min_x: usize,
        max_x: usize,
        min_y: usize,
        max_y: usize,
    }

    let min_area = ((ch_c * cw_c) as f32 * 0.0045).floor().max(5.0) as usize;
    let min_h = ((ch_c as f32) * 0.22).floor().max(8.0) as usize;
    let max_h = ((ch_c as f32) * 0.78) as usize;
    let max_w = ((cw_c as f32) * 0.48) as usize;

    let mut candidates: Vec<CompCandidate> = Vec::new();

    for y in 0..ch_c {
        for x in 0..cw_c {
            let start_idx = y * cw_c + x;
            if !white_mask[start_idx] || labels[start_idx] != 0 {
                continue;
            }

            current_label += 1;
            let lbl = current_label;
            labels[start_idx] = lbl;
            queue.push_back((x, y));

            let mut pixels = Vec::new();
            let mut min_x = x;
            let mut max_x = x;
            let mut min_y = y;
            let mut max_y = y;

            while let Some((px, py)) = queue.pop_front() {
                pixels.push((px, py));
                min_x = min_x.min(px);
                max_x = max_x.max(px);
                min_y = min_y.min(py);
                max_y = max_y.max(py);

                // Conectividade-4 ortogonal (impede pontes diagonais de 1px entre dígitos e ondas brancas do elmo)
                let neighbors = [
                    (px.wrapping_sub(1), py),
                    (px + 1, py),
                    (px, py.wrapping_sub(1)),
                    (px, py + 1),
                ];
                for (nx, ny) in neighbors {
                    if nx < cw_c && ny < ch_c {
                        let n_idx = ny * cw_c + nx;
                        if white_mask[n_idx] && labels[n_idx] == 0 {
                            labels[n_idx] = lbl;
                            queue.push_back((nx, ny));
                        }
                    }
                }
            }

            let area = pixels.len();
            let comp_w = max_x - min_x + 1;
            let comp_h = max_y - min_y + 1;

            if min_x <= 1 || min_y <= 1 || max_x + 1 >= cw_c - 1 || max_y + 1 >= ch_c - 1 {
                continue;
            }
            if area < min_area || comp_h < min_h || comp_h > max_h || comp_w > max_w {
                continue;
            }
            if (comp_w as f32) / (comp_h as f32) > 1.25 {
                continue;
            }
            let fill_ratio = (area as f32) / ((comp_w * comp_h) as f32);
            if fill_ratio < 0.25 {
                continue;
            }

            let cy_f = min_y as f32 + comp_h as f32 * 0.5;
            let cx_f = min_x as f32 + comp_w as f32 * 0.5;
            if cy_f < 0.22 * (ch_c as f32) || cy_f > 0.80 * (ch_c as f32) {
                continue;
            }

            // Verifica a sombra projetada direcional inferior-direita (bottom-right: dx em 0..=2, dy em 0..=2).
            let by0 = min_y;
            let by1 = (max_y + 2).min(ch_c - 1);
            let bx0 = min_x;
            let bx1 = (max_x + 2).min(cw_c - 1);
            let bw = bx1 - bx0 + 1;
            let bh = by1 - by0 + 1;

            let mut br_shadow = vec![false; bw * bh];
            for &(px, py) in &pixels {
                for dy in 0..=2usize {
                    for dx in 0..=2usize {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = px + dx;
                        let ny = py + dy;
                        if nx <= bx1 && ny <= by1 {
                            br_shadow[(ny - by0) * bw + (nx - bx0)] = true;
                        }
                    }
                }
            }

            let mut shadow_vs = Vec::new();
            for ny in by0..=by1 {
                for nx in bx0..=bx1 {
                    if br_shadow[(ny - by0) * bw + (nx - bx0)] && labels[ny * cw_c + nx] != lbl {
                        shadow_vs.push(val_map[ny * cw_c + nx]);
                    }
                }
            }

            if shadow_vs.is_empty() {
                continue;
            }

            let dark_px = shadow_vs.iter().filter(|&&v| v < 105).count();
            let dark_ratio = (dark_px as f32) / (shadow_vs.len() as f32);
            let min_v = *shadow_vs.iter().min().unwrap_or(&255);

            // Aceita dígitos com sombra até min_v <= 68 (como o '1' fino sobre a linha do relógio no Manto de Clérigo)
            if dark_px >= 5 && dark_ratio >= 0.18 && min_v <= 68 {
                let dist_center =
                    (cx_f - cw_c as f32 * 0.5).abs() + (cy_f - ch_c as f32 * 0.5).abs();
                candidates.push(CompCandidate {
                    label: lbl,
                    cy: cy_f,
                    ch: comp_h as f32,
                    dist_center,
                    min_v,
                    min_x,
                    max_x,
                    min_y,
                    max_y,
                });
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by(|a, b| a.dist_center.partial_cmp(&b.dist_center).unwrap());
    // Exige pelo menos um dígito âncora forte com sombra escura nítida (min_v <= 45),
    // garantindo 0 falsos positivos em ícones prontos e permitindo incluir dígitos irmãos alinhados (min_v <= 68).
    let anchor = candidates.iter().find(|c| c.min_v <= 45)?;
    let ref_cy = anchor.cy;
    let ref_ch = anchor.ch;

    let mut aligned_labels = Vec::new();
    let mut box_x0 = cw_c;
    let mut box_x1 = 0usize;
    let mut box_y0 = ch_c;
    let mut box_y1 = 0usize;

    for c in &candidates {
        if (c.cy - ref_cy).abs() <= (0.35 * ref_ch).max(4.0)
            && c.ch >= 0.65 * ref_ch
            && c.ch <= 1.45 * ref_ch
        {
            aligned_labels.push(c.label);
            box_x0 = box_x0.min(c.min_x);
            box_x1 = box_x1.max(c.max_x);
            box_y0 = box_y0.min(c.min_y);
            box_y1 = box_y1.max(c.max_y);
        }
    }

    if aligned_labels.is_empty() || box_x1 < box_x0 || box_y1 < box_y0 {
        return None;
    }

    let raw_dw = box_x1 - box_x0 + 1;
    let raw_dh = box_y1 - box_y0 + 1;

    let mut inv_box = vec![255u8; raw_dw * raw_dh];
    for y in box_y0..=box_y1 {
        for x in box_x0..=box_x1 {
            let lbl = labels[y * cw_c + x];
            if aligned_labels.contains(&lbl) {
                inv_box[(y - box_y0) * raw_dw + (x - box_x0)] = 0;
            }
        }
    }

    let up_w = raw_dw * 2;
    let up_h = raw_dh * 2;
    let mut up_digit = vec![255u8; up_w * up_h];
    for uy in 0..up_h {
        let fy = (uy as f32 + 0.5) * 0.5 - 0.5;
        let y0 = fy.floor().clamp(0.0, (raw_dh - 1) as f32) as usize;
        let y1 = (y0 + 1).min(raw_dh - 1);
        let wy = (fy - fy.floor()).clamp(0.0, 1.0);

        for ux in 0..up_w {
            let fx = (ux as f32 + 0.5) * 0.5 - 0.5;
            let x0 = fx.floor().clamp(0.0, (raw_dw - 1) as f32) as usize;
            let x1 = (x0 + 1).min(raw_dw - 1);
            let wx = (fx - fx.floor()).clamp(0.0, 1.0);

            let p00 = inv_box[y0 * raw_dw + x0] as f32;
            let p10 = inv_box[y0 * raw_dw + x1] as f32;
            let p01 = inv_box[y1 * raw_dw + x0] as f32;
            let p11 = inv_box[y1 * raw_dw + x1] as f32;

            let top = p00 * (1.0 - wx) + p10 * wx;
            let bot = p01 * (1.0 - wx) + p11 * wx;
            up_digit[uy * up_w + ux] = (top * (1.0 - wy) + bot * wy).round() as u8;
        }
    }

    let pad = 20usize;
    let gap = ((up_h as f32) * 0.55).round().max(18.0) as usize;
    // Repete o bloco de dígitos 3x lado a lado ("27  27  27" ou "7  7  7"):
    // O Windows OCR ignora 1 caractere isolado na tela, mas sempre reconhece
    // perfeitamente quando há 3 blocos na mesma linha usando a própria fonte do jogo!
    let copies = if aligned_labels.len() == 1 { 3usize } else { 2usize };
    let canvas_h = up_h.max(24) + pad * 2;
    let canvas_w = pad * 2 + up_w * copies + gap * (copies - 1);

    let mut gray_canvas = vec![255u8; canvas_w * canvas_h];
    let y_top = (canvas_h - up_h) / 2;

    for copy_idx in 0..copies {
        let digit_x0 = pad + copy_idx * (up_w + gap);
        for uy in 0..up_h {
            for ux in 0..up_w {
                if digit_x0 + ux < canvas_w {
                    gray_canvas[(y_top + uy) * canvas_w + (digit_x0 + ux)] =
                        up_digit[uy * up_w + ux];
                }
            }
        }
    }

    let mut bgra_canvas = vec![255u8; canvas_w * canvas_h * 4];
    for (i, &g) in gray_canvas.iter().enumerate() {
        bgra_canvas[i * 4] = g;
        bgra_canvas[i * 4 + 1] = g;
        bgra_canvas[i * 4 + 2] = g;
        bgra_canvas[i * 4 + 3] = 255;
    }

    Some((canvas_w, canvas_h, bgra_canvas, aligned_labels.len()))
}

pub fn create_windows_ocr_engine() -> Option<OcrEngine> {
    if let Ok(eng) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Some(eng);
    }
    for code in ["pt-BR", "en-US", "es-ES"] {
        if let Ok(lang) = Language::CreateLanguage(&HSTRING::from(code)) {
            if let Ok(eng) = OcrEngine::TryCreateFromLanguage(&lang) {
                return Some(eng);
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOcrStatus {
    /// Número inteiro de cooldown reconhecido pelo OCR (`1..=600`).
    Cooldown(u32),
    /// Décimos finais `< 1.0s` (ex: `"0.9"`..`"0.1"` ou `"09"`..`"01"`), indicando fim imediato da recarga.
    SubSecondDecimal,
    /// O detector geométrico encontrou dígito branco central com sombra preta nítida,
    /// mas o OCR não reconheceu o caractere naquele frame (evita piscar no meio do cooldown).
    DigitShapeOnly,
    /// Nenhum dígito de cooldown presente no centro do slot.
    Empty,
}

pub fn read_slot_status_bgra(
    engine: &OcrEngine,
    slot_w: usize,
    slot_h: usize,
    slot_bgra: &[u8],
) -> SlotOcrStatus {
    let (cw, ch, canvas_bgra, num_components) =
        match prepare_ocr_canvas_bgra(slot_w, slot_h, slot_bgra) {
            Some(v) => v,
            None => return SlotOcrStatus::Empty,
        };

    let ibuffer = match CryptographicBuffer::CreateFromByteArray(&canvas_bgra) {
        Ok(b) => b,
        Err(_) => return SlotOcrStatus::DigitShapeOnly,
    };
    let bitmap = match SoftwareBitmap::CreateCopyFromBuffer(
        &ibuffer,
        BitmapPixelFormat::Bgra8,
        cw as i32,
        ch as i32,
    ) {
        Ok(bmp) => bmp,
        Err(_) => return SlotOcrStatus::DigitShapeOnly,
    };

    let raw_text = match engine
        .RecognizeAsync(&bitmap)
        .ok()
        .and_then(|op| op.get().ok())
        .and_then(|res| res.Text().ok())
    {
        Some(t) => t.to_string(),
        None => return SlotOcrStatus::DigitShapeOnly,
    };

    parse_ocr_cooldown_status(&raw_text, num_components)
}

pub fn read_slot_seconds_bgra(
    engine: &OcrEngine,
    slot_w: usize,
    slot_h: usize,
    slot_bgra: &[u8],
) -> Option<u32> {
    match read_slot_status_bgra(engine, slot_w, slot_h, slot_bgra) {
        SlotOcrStatus::Cooldown(sec) => Some(sec),
        _ => None,
    }
}

#[allow(dead_code)]
fn parse_ocr_cooldown_text(raw_text: &str, num_components: usize) -> Option<u32> {
    match parse_ocr_cooldown_status(raw_text, num_components) {
        SlotOcrStatus::Cooldown(sec) => Some(sec),
        _ => None,
    }
}

fn parse_ocr_cooldown_status(raw_text: &str, num_components: usize) -> SlotOcrStatus {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return SlotOcrStatus::DigitShapeOnly;
    }

    if let Ok(re_min) = Regex::new(r"(?i)(\d+)\s*m\b") {
        if let Some(cap) = re_min.captures(trimmed) {
            if let Ok(mins) = cap[1].parse::<u32>() {
                return SlotOcrStatus::Cooldown(mins * 60);
            }
        }
    }

    let norm: String = trimmed
        .chars()
        .map(|c| match c {
            'O' | 'o' => '0',
            'I' | 'l' | '|' | 'i' | '!' => '1',
            'Z' | 'z' => '2',
            'S' | 's' => '5',
            'B' => '8',
            other => other,
        })
        .collect();

    // Se o detector geométrico encontrou exatamente 1 componente branco centralizado,
    // o cooldown na tela tem apenas 1 dígito (1..9), que foi repetido 3x lado a lado no canvas.
    if num_components == 1 {
        for c in norm.chars() {
            if let Some(d) = c.to_digit(10) {
                if (1..=9).contains(&d) {
                    return SlotOcrStatus::Cooldown(d);
                }
            }
        }
        return SlotOcrStatus::DigitShapeOnly;
    }

    if let Ok(re_dec) = Regex::new(r"(\d+)[.,](\d+)") {
        if let Some(cap) = re_dec.captures(&norm) {
            if let Ok(whole) = cap[1].parse::<u32>() {
                return if whole >= 1 {
                    SlotOcrStatus::Cooldown(whole)
                } else {
                    SlotOcrStatus::SubSecondDecimal
                };
            }
        }
    }

    if let Ok(re_num) = Regex::new(r"\d+") {
        if let Some(m) = re_num.find(&norm) {
            let mut s = m.as_str();
            // Caso as cópias lado a lado tenham sido lidas juntas sem espaço (ex: "2727" quando num_components == 2)
            if s.len() > num_components && num_components >= 2 && s.len() % num_components == 0 {
                let first_part = &s[..num_components];
                if s == first_part.repeat(s.len() / num_components) {
                    s = first_part;
                }
            }
            // Quando o Albion exibe < 1.0s (ex: "0.9" .. "0.1"), o ponto decimal é filtrado por área mínima
            // e o OCR lê "09" .. "01" (2 dígitos começando com '0').
            if s.starts_with('0') {
                return if s.len() >= 2 {
                    SlotOcrStatus::SubSecondDecimal
                } else {
                    SlotOcrStatus::DigitShapeOnly
                };
            }
            if let Ok(val) = s.parse::<u32>() {
                if (1..=600).contains(&val) {
                    return SlotOcrStatus::Cooldown(val);
                }
            }
        }
    }

    SlotOcrStatus::DigitShapeOnly
}

/// Captura um snapshot pontual (para exibir preview estático após selecionar a barra).
pub fn capture_single_preview_snapshot(state: &VisionStateHandle) -> VisionStateSnapshot {
    let (bbox, is_tracking, active_arr) = {
        let guard = state.lock().unwrap();
        (guard.bbox, guard.is_tracking, guard.slot_active)
    };

    let mut previews = Vec::new();
    if let Some(b) = bbox {
        if let Some((bw, bh, bar_bgra)) = grab_screen_region_bgra(b) {
            let slots_bgra = split_bar_into_slots_bgra(bw, bh, &bar_bgra);
            let engine = create_windows_ocr_engine();

            for (i, &(key, field, label)) in SLOTS.iter().enumerate() {
                let (sw, sh, ref sbuf) = slots_bgra[i];
                let secs = engine
                    .as_ref()
                    .and_then(|eng| read_slot_seconds_bgra(eng, sw, sh, sbuf));
                let active = if is_tracking {
                    active_arr[i]
                } else {
                    secs.is_some()
                };
                previews.push(SlotPreview {
                    key: key.to_string(),
                    field: field.to_string(),
                    label: label.to_string(),
                    active,
                    seconds: secs,
                    image_data_url: encode_slot_thumbnail_data_url(sw, sh, sbuf),
                });
            }
        }
    }

    VisionStateSnapshot {
        bbox,
        is_tracking,
        slots: previews,
    }
}

/// Loop contínuo em background (70ms / ~14.3 FPS) que monitora a barra do Albion e emite eventos ao Tauri.
pub fn spawn_vision_loop(app: tauri::AppHandle, state: VisionStateHandle) {
    use tauri::Emitter;

    std::thread::spawn(move || {
        let engine = match create_windows_ocr_engine() {
            Some(e) => e,
            None => {
                log::error!("Falha ao inicializar Windows.Media.Ocr.OcrEngine");
                return;
            }
        };

        let mut frame_counter: u32 = 0;

        loop {
            let (bbox_opt, is_tracking, is_locked) = {
                let guard = state.lock().unwrap();
                (guard.bbox, guard.is_tracking, guard.is_overlay_locked)
            };

            if !is_tracking || bbox_opt.is_none() {
                std::thread::sleep(Duration::from_millis(150));
                continue;
            }

            let bbox = bbox_opt.unwrap();
            if let Some((bw, bh, bar_bgra)) = grab_screen_region_bgra(bbox) {
                let slots_bgra = split_bar_into_slots_bgra(bw, bh, &bar_bgra);
                let mut statuses = [SlotOcrStatus::Empty; 5];
                let mut raw_secs = [None; 5];
                for i in 0..5 {
                    let (sw, sh, ref sbuf) = slots_bgra[i];
                    let st = read_slot_status_bgra(&engine, sw, sh, sbuf);
                    statuses[i] = st;
                    if let SlotOcrStatus::Cooldown(sec) = st {
                        raw_secs[i] = Some(sec);
                    }
                }

                let mut changed = HashMap::new();
                let now = Instant::now();
                let current_active = {
                    let mut guard = state.lock().unwrap();
                    for i in 0..5 {
                        let field = SLOTS[i].1;
                        let currently_in_cd = guard.slot_active[i];
                        let elapsed_since_hit =
                            now.duration_since(guard.last_hit_at[i]).as_secs_f32();
                        let effective_rem = (guard.last_read_sec[i] as f32) - elapsed_since_hit;

                        match statuses[i] {
                            SlotOcrStatus::Cooldown(sec) => {
                                guard.miss_streak[i] = 0;
                                guard.hit_streak[i] = guard.hit_streak[i].saturating_add(1);

                                // Proteção contra queda abrupta de 2 dígitos para 1 dígito (ex: ler '14' como '4' ou '1' em 1 frame):
                                // Se já estava em cooldown com > 8s restantes e o valor caiu >= 6.5s de repente,
                                // exige 5 leituras baixas seguidas (~0.35s) antes de reduzir `last_read_sec`.
                                if currently_in_cd && (effective_rem - (sec as f32)) >= 6.5 {
                                    guard.low_jump_streak[i] =
                                        guard.low_jump_streak[i].saturating_add(1);
                                    if guard.low_jump_streak[i] >= 5 {
                                        guard.low_jump_streak[i] = 0;
                                        guard.last_read_sec[i] = sec;
                                        guard.last_hit_at[i] = now;
                                    }
                                } else {
                                    guard.low_jump_streak[i] = 0;
                                    guard.last_read_sec[i] = sec;
                                    guard.last_hit_at[i] = now;
                                }

                                if !currently_in_cd && (sec >= 2 || guard.hit_streak[i] >= 2) {
                                    guard.slot_active[i] = true;
                                    changed.insert(field.to_string(), true);
                                }
                            }
                            SlotOcrStatus::DigitShapeOnly => {
                                guard.hit_streak[i] = 0;
                                guard.low_jump_streak[i] = 0;
                                if currently_in_cd {
                                    // O número branco com sombra preta continua fisicamente visível no centro do slot!
                                    // Se ainda faltam > 1.5s, não contamos como perda de visão (evita piscar em '4', '10', etc.).
                                    if effective_rem <= 1.5 {
                                        guard.miss_streak[i] =
                                            guard.miss_streak[i].saturating_add(1);
                                        if guard.miss_streak[i] >= 2 {
                                            guard.slot_active[i] = false;
                                            guard.miss_streak[i] = 0;
                                            guard.last_read_sec[i] = 0;
                                            changed.insert(field.to_string(), false);
                                        }
                                    } else {
                                        guard.miss_streak[i] = 0;
                                    }
                                }
                            }
                            SlotOcrStatus::SubSecondDecimal => {
                                guard.hit_streak[i] = 0;
                                guard.low_jump_streak[i] = 0;
                                if currently_in_cd {
                                    // Nos últimos segundos (<= 2.5s), '0.9'..'0.1' libera em 2 frames (~0.14s)!
                                    if effective_rem <= 2.5 {
                                        guard.miss_streak[i] =
                                            guard.miss_streak[i].saturating_add(1);
                                        if guard.miss_streak[i] >= 2 {
                                            guard.slot_active[i] = false;
                                            guard.miss_streak[i] = 0;
                                            guard.last_read_sec[i] = 0;
                                            changed.insert(field.to_string(), false);
                                        }
                                    }
                                }
                            }
                            SlotOcrStatus::Empty => {
                                guard.hit_streak[i] = 0;
                                guard.low_jump_streak[i] = 0;
                                if currently_in_cd {
                                    guard.miss_streak[i] = guard.miss_streak[i].saturating_add(1);
                                    // Quando o cooldown está acabando (effective_rem <= 2.2s), libera em 2 frames (~0.14s).
                                    // No meio da recarga (effective_rem > 2.2s), exige 20 frames (~1.4s) para nunca piscar
                                    // caso o mouse ou efeito visual passe sobre a barra!
                                    let min_misses = if effective_rem <= 2.2 { 2 } else { 20 };
                                    if guard.miss_streak[i] >= min_misses {
                                        guard.slot_active[i] = false;
                                        guard.miss_streak[i] = 0;
                                        guard.last_read_sec[i] = 0;
                                        changed.insert(field.to_string(), false);
                                    }
                                }
                            }
                        }
                    }
                    guard.slot_active
                };

                if !changed.is_empty() {
                    let _ = app.emit(
                        "cv-cooldown-change",
                        CooldownChangePayload { changes: changed },
                    );
                }

                frame_counter = frame_counter.wrapping_add(1);
                if !is_locked && frame_counter % 5 == 0 {
                    let mut previews = Vec::with_capacity(5);
                    for i in 0..5 {
                        let (key, field, label) = SLOTS[i];
                        let (sw, sh, ref sbuf) = slots_bgra[i];
                        previews.push(SlotPreview {
                            key: key.to_string(),
                            field: field.to_string(),
                            label: label.to_string(),
                            active: current_active[i],
                            seconds: raw_secs[i],
                            image_data_url: encode_slot_thumbnail_data_url(sw, sh, sbuf),
                        });
                    }
                    let _ = app.emit(
                        "cv-preview",
                        VisionStateSnapshot {
                            bbox: Some(bbox),
                            is_tracking: true,
                            slots: previews,
                        },
                    );
                }
            }

            std::thread::sleep(Duration::from_millis(70));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn load_png_as_bgra(path: &str) -> (usize, usize, Vec<u8>) {
        let decoder = png::Decoder::new(File::open(path).expect("PNG file not found"));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).unwrap();
        let w = info.width as usize;
        let h = info.height as usize;
        let bytes = &buf[..info.buffer_size()];
        let mut bgra = vec![255u8; w * h * 4];

        match info.color_type {
            png::ColorType::Rgba => {
                for i in 0..(w * h) {
                    bgra[i * 4] = bytes[i * 4 + 2];
                    bgra[i * 4 + 1] = bytes[i * 4 + 1];
                    bgra[i * 4 + 2] = bytes[i * 4];
                    bgra[i * 4 + 3] = bytes[i * 4 + 3];
                }
            }
            png::ColorType::Rgb => {
                for i in 0..(w * h) {
                    bgra[i * 4] = bytes[i * 3 + 2];
                    bgra[i * 4 + 1] = bytes[i * 3 + 1];
                    bgra[i * 4 + 2] = bytes[i * 3];
                    bgra[i * 4 + 3] = 255;
                }
            }
            _ => panic!("Unsupported color type in test PNG"),
        }
        (w, h, bgra)
    }

    #[test]
    fn test_albion_ocr_all_cases() {
        let engine = create_windows_ocr_engine().expect("Windows OCR engine must initialize");

        // 1. Ultimate 27s (início da recarga, fundo escuro)
        let img2_path = r"C:/Users/Vidotto/.gemini/antigravity/brain/d60d0b86-7286-4452-8439-dad9a3cf2f97/.user_uploaded/media_1793651922151_580f5d24.png";
        if std::path::Path::new(img2_path).exists() {
            let (w, h, bgra) = load_png_as_bgra(img2_path);
            assert_eq!(read_slot_seconds_bgra(&engine, w, h, &bgra), Some(27));

            // Simula dígito único '2' apagando a metade direita do centro (onde fica o '7')
            let mut only_2 = bgra.clone();
            for y in (h * 25 / 100)..(h * 73 / 100) {
                for x in (w * 50 / 100)..(w * 84 / 100) {
                    let i = (y * w + x) * 4;
                    only_2[i] = 20;
                    only_2[i + 1] = 20;
                    only_2[i + 2] = 20;
                }
            }
            let r2 = read_slot_seconds_bgra(&engine, w, h, &only_2);
            println!("Single digit '2' read as: {:?}", r2);
            assert_eq!(r2, Some(2));

            // Simula dígito único '7' apagando a metade esquerda do centro (onde fica o '2')
            let mut only_7 = bgra.clone();
            for y in (h * 25 / 100)..(h * 73 / 100) {
                for x in (w * 16 / 100)..(w * 51 / 100) {
                    let i = (y * w + x) * 4;
                    only_7[i] = 20;
                    only_7[i + 1] = 20;
                    only_7[i + 2] = 20;
                }
            }
            let r7 = read_slot_seconds_bgra(&engine, w, h, &only_7);
            println!("Single digit '7' read as: {:?}", r7);
            assert_eq!(r7, Some(7));
        }

        // 2. Manto de Clérigo 13s (quase 100% recuperado, fundo azul brilhante + nick branco embaixo)
        let img3_path = r"C:/Users/Vidotto/.gemini/antigravity/brain/d60d0b86-7286-4452-8439-dad9a3cf2f97/.user_uploaded/media_1793656155848_1230b6e9.png";
        if std::path::Path::new(img3_path).exists() {
            let (w, h, bgra) = load_png_as_bgra(img3_path);
            assert_eq!(read_slot_seconds_bgra(&engine, w, h, &bgra), Some(13));

            // Simula dígito único '1' apagando a metade direita do centro (onde fica o '3')
            let mut only_1 = bgra.clone();
            for y in (h * 25 / 100)..(h * 73 / 100) {
                for x in (w * 48 / 100)..(w * 84 / 100) {
                    let i = (y * w + x) * 4;
                    only_1[i] = 20;
                    only_1[i + 1] = 20;
                    only_1[i + 2] = 20;
                }
            }
            let r1 = read_slot_seconds_bgra(&engine, w, h, &only_1);
            println!("Single digit '1' read as: {:?}", r1);
            assert_eq!(r1, Some(1));

            // Simula dígito único '3' apagando a metade esquerda do centro (onde fica o '1')
            let mut only_3 = bgra.clone();
            for y in (h * 25 / 100)..(h * 73 / 100) {
                for x in (w * 16 / 100)..(w * 49 / 100) {
                    let i = (y * w + x) * 4;
                    only_3[i] = 20;
                    only_3[i + 1] = 20;
                    only_3[i + 2] = 20;
                }
            }
            let r3 = read_slot_seconds_bgra(&engine, w, h, &only_3);
            println!("Single digit '3' read as: {:?}", r3);
            assert_eq!(r3, Some(3));
        }

        // 3. Barra completa com os 5 slots prontos (E, R, D, F, 1) -> todos devem retornar None (0 falsos positivos)
        let bar_path = r"C:/Users/Vidotto/.gemini/antigravity/brain/d60d0b86-7286-4452-8439-dad9a3cf2f97/scratch/bar_e_to_1.png";
        if std::path::Path::new(bar_path).exists() {
            let (bw, bh, bar_bgra) = load_png_as_bgra(bar_path);
            let slots = split_bar_into_slots_bgra(bw, bh, &bar_bgra);
            for (idx, (sw, sh, sbuf)) in slots.iter().enumerate() {
                let res = read_slot_seconds_bgra(&engine, *sw, *sh, sbuf);
                println!("Ready slot {} ({}) -> {:?}", idx, SLOTS[idx].0, res);
                assert_eq!(res, None);
            }
        }

        // 4. Print do Elmo com ondas brancas (D = 5s) + Manto de Clérigo (R = 14s) + E/F prontos
        let r14_path = r"C:/Users/Vidotto/.gemini/antigravity/brain/d60d0b86-7286-4452-8439-dad9a3cf2f97/scratch/slot_R_14.png";
        if std::path::Path::new(r14_path).exists() {
            let (w, h, bgra) = load_png_as_bgra(r14_path);
            let res = read_slot_seconds_bgra(&engine, w, h, &bgra);
            println!("slot_R_14 read as: {:?}", res);
            assert_eq!(res, Some(14));
        }

        let d5_path = r"C:/Users/Vidotto/.gemini/antigravity/brain/d60d0b86-7286-4452-8439-dad9a3cf2f97/scratch/slot_D_5.png";
        if std::path::Path::new(d5_path).exists() {
            let (w, h, bgra) = load_png_as_bgra(d5_path);
            let res = read_slot_seconds_bgra(&engine, w, h, &bgra);
            println!("slot_D_5 read as: {:?}", res);
            assert_eq!(res, Some(5));
        }

        for ready_name in ["slot_E.png", "slot_F.png"] {
            let p = format!(
                "C:/Users/Vidotto/.gemini/antigravity/brain/d60d0b86-7286-4452-8439-dad9a3cf2f97/scratch/{}",
                ready_name
            );
            if std::path::Path::new(&p).exists() {
                let (w, h, bgra) = load_png_as_bgra(&p);
                let res = read_slot_seconds_bgra(&engine, w, h, &bgra);
                println!("{} read as: {:?}", ready_name, res);
                assert_eq!(res, None);
            }
        }

        // 5. Décimos de segundo no final do cooldown ("0.3" -> "03" sem ponto) devem retornar None (liberação imediata)
        assert_eq!(parse_ocr_cooldown_text("03 03", 2), None);
        assert_eq!(parse_ocr_cooldown_text("0.4 0.4", 2), None);
    }
}
