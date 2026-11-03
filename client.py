import os
import re
import json
import time
import ctypes
from ctypes import wintypes
import threading
import tkinter as tk
from tkinter import ttk, messagebox
import cv2
import numpy as np
import requests
from PIL import Image, ImageGrab, ImageTk
import winocr

# ==============================================================================
# DPI Awareness (garante coordenadas 1:1 com os pixels reais da tela no Windows)
# ==============================================================================
try:
    ctypes.windll.shcore.SetProcessDpiAwareness(2)  # Per-monitor DPI aware
except Exception:
    try:
        ctypes.windll.user32.SetProcessDPIAware()
    except Exception:
        pass

CONFIG_FILE = "config_cv.json"
FIREBASE_DB_URL = "https://albioncd-e2f3b-default-rtdb.firebaseio.com"

# Ordem exata na barra de skills do Albion Online (da esquerda para a direita):
# E (Ultimate) -> R (Peito) -> D (Elmo) -> F (Bota) -> 1 (Poção)
SLOTS = [
    {"key": "E", "field": "ultimate", "label": "Ult (E)"},
    {"key": "R", "field": "peito",    "label": "Peito (R)"},
    {"key": "D", "field": "elmo",     "label": "Elmo (D)"},
    {"key": "F", "field": "bota",     "label": "Bota (F)"},
    {"key": "1", "field": "pocao",    "label": "Poção (1)"},
]

# Ordem de exibição dos ícones nos cards da Party (igual ao CdPage original)
OVERLAY_ICON_ORDER = [
    ("bota",     "⚡", "Bota (F)"),
    ("peito",    "🛡", "Peito (R)"),
    ("elmo",     "🪖", "Elmo (D)"),
    ("pocao",    "🧪", "Poção (1)"),
    ("ultimate", "★",  "Ultimate (E)"),
]

CARD_COLORS = {
    "Padrão":   "#1f2937",
    "Verde":    "#14532d",
    "Azul":     "#1e3a8a",
    "Vermelho": "#7f1d1d",
    "Amarelo":  "#713f12",
}


# ==============================================================================
# Captura rápida de tela via Win32 GDI BitBlt (ignora overlays transparentes!)
# ==============================================================================
class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", wintypes.LONG),
        ("biHeight", wintypes.LONG),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", wintypes.LONG),
        ("biYPelsPerMeter", wintypes.LONG),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [
        ("bmiHeader", BITMAPINFOHEADER),
        ("bmiColors", wintypes.DWORD * 3),
    ]


SRCCOPY = 0x00CC0020
DIB_RGB_COLORS = 0


def grab_screen_region_bgr(bbox):
    """
    Captura apenas o retângulo (x1, y1, x2, y2) em BGR (OpenCV).
    Usa Win32 BitBlt sem CAPTUREBLT: muito mais rápido (<1ms) e NÃO captura
    janelas transparentes de overlay que estejam por cima.
    """
    x1, y1, x2, y2 = [int(v) for v in bbox]
    w = x2 - x1
    h = y2 - y1
    if w <= 4 or h <= 4:
        return None

    user32 = ctypes.windll.user32
    gdi32 = ctypes.windll.gdi32

    hwnd_desktop = user32.GetDesktopWindow()
    hdc_screen = user32.GetWindowDC(hwnd_desktop)
    if not hdc_screen:
        shot = ImageGrab.grab(bbox=(x1, y1, x2, y2), all_screens=True)
        return cv2.cvtColor(np.array(shot), cv2.COLOR_RGB2BGR)

    hdc_mem = gdi32.CreateCompatibleDC(hdc_screen)
    hbmp = gdi32.CreateCompatibleBitmap(hdc_screen, w, h)
    old_obj = gdi32.SelectObject(hdc_mem, hbmp)

    try:
        ok = gdi32.BitBlt(hdc_mem, 0, 0, w, h, hdc_screen, x1, y1, SRCCOPY)
        if not ok:
            raise RuntimeError("BitBlt failed")

        bmi = BITMAPINFO()
        bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bmi.bmiHeader.biWidth = w
        bmi.bmiHeader.biHeight = -h  # Top-down
        bmi.bmiHeader.biPlanes = 1
        bmi.bmiHeader.biBitCount = 32
        bmi.bmiHeader.biCompression = 0

        buf = ctypes.create_string_buffer(w * h * 4)
        lines = gdi32.GetDIBits(hdc_mem, hbmp, 0, h, buf, ctypes.byref(bmi), DIB_RGB_COLORS)
        if lines != h:
            raise RuntimeError("GetDIBits failed")

        arr = np.frombuffer(buf, dtype=np.uint8).reshape((h, w, 4))
        return arr[:, :, :3].copy()
    except Exception:
        shot = ImageGrab.grab(bbox=(x1, y1, x2, y2), all_screens=True)
        return cv2.cvtColor(np.array(shot), cv2.COLOR_RGB2BGR)
    finally:
        gdi32.SelectObject(hdc_mem, old_obj)
        gdi32.DeleteObject(hbmp)
        gdi32.DeleteDC(hdc_mem)
        user32.ReleaseDC(hwnd_desktop, hdc_screen)


# ==============================================================================
# Motor de Visão Computacional + Windows OCR para Cooldowns do Albion Online
# ==============================================================================
def detect_supported_ocr_lang():
    """Detecta automaticamente o idioma de OCR instalado no Windows."""
    dummy = np.full((40, 80, 3), 255, dtype=np.uint8)
    for lang in ("pt-BR", "en-US", "es-ES", "fr-FR", "de-DE"):
        try:
            winocr.recognize_cv2_sync(dummy, lang)
            return lang
        except Exception:
            continue
    return "pt-BR"


class AlbionCooldownCV:
    """
    Lê números de tempo de recarga nos slots de skill do Albion Online
    independentemente da arte de fundo da skill.
    """

    def __init__(self):
        self.ocr_lang = detect_supported_ocr_lang()

    def read_slot_seconds(self, slot_bgr: np.ndarray):
        """
        Retorna os segundos detectados (int >= 1) se o slot estiver em cooldown,
        ou None caso não haja número de cooldown visível.
        """
        if slot_bgr is None:
            return None
        h, w = slot_bgr.shape[:2]
        if h < 16 or w < 16:
            return None

        # Recorta apenas o centro onde o número de cooldown aparece.
        # Isso ignora automaticamente:
        #  - quantidade de poções/comida no topo ("x10")
        #  - letra do atalho no canto inferior direito ("E", "R", "D", "F", "1")
        #  - borda metálica circular do slot
        y0_c, y1_c = int(h * 0.25), int(h * 0.73)
        x0_c, x1_c = int(w * 0.16), int(w * 0.84)
        crop = slot_bgr[y0_c:y1_c, x0_c:x1_c]
        ch_c, cw_c = crop.shape[:2]
        if ch_c < 10 or cw_c < 10:
            return None

        hsv = cv2.cvtColor(crop, cv2.COLOR_BGR2HSV)

        # Máscara de branco puro dos dígitos (separa de fundos azuis/claros e ondas brancas do elmo)
        white_mask = cv2.inRange(hsv, (0, 0, 218), (180, 29, 255))

        num_labels, labels, stats, _ = cv2.connectedComponentsWithStats(white_mask, connectivity=4)
        if num_labels <= 1:
            return None

        min_area = max(5, int(ch_c * cw_c * 0.0045))
        min_h = max(8, int(ch_c * 0.22))
        max_h = int(ch_c * 0.78)
        max_w = int(cw_c * 0.48)

        candidates = []
        for i in range(1, num_labels):
            x, y, cw, ch, area = stats[i]
            if x <= 1 or y <= 1 or (x + cw) >= cw_c - 1 or (y + ch) >= ch_c - 1:
                continue
            if area < min_area:
                continue
            if not (min_h <= ch <= max_h):
                continue
            if cw > max_w or (cw / float(ch)) > 1.25:
                continue
            if (area / float(cw * ch)) < 0.25:
                continue

            cy = y + ch / 2.0
            cx = x + cw / 2.0
            if not (0.22 * ch_c <= cy <= 0.80 * ch_c):
                continue

            # Verifica sombra direcional projetada inferior-direita (bottom-right: dx em 0..2, dy em 0..2).
            comp = labels == i
            br_shadow = np.zeros_like(comp)
            for dy in (0, 1, 2):
                for dx in (0, 1, 2):
                    if dx == 0 and dy == 0:
                        continue
                    ys_end = ch_c - dy if dy > 0 else ch_c
                    xs_end = cw_c - dx if dx > 0 else cw_c
                    br_shadow[dy:, dx:] |= comp[:ys_end, :xs_end]
            br_shadow &= ~comp
            if not np.any(br_shadow):
                continue

            shadow_v = hsv[:, :, 2][br_shadow]
            dark_px = int(np.sum(shadow_v < 105))
            dark_ratio = float(np.mean(shadow_v < 105))
            min_v = int(shadow_v.min())

            if dark_px >= 5 and dark_ratio >= 0.18 and min_v <= 68:
                dist_center = abs(cx - cw_c / 2.0) + abs(cy - ch_c / 2.0)
                candidates.append((i, cx, cy, cw, ch, dist_center, min_v))

        # Fast-path: sem candidatos válidos -> nem precisa chamar OCR (< 0.2ms)
        if not candidates:
            return None

        # Exige pelo menos um dígito âncora forte com sombra escura nítida (min_v <= 45)
        candidates.sort(key=lambda item: item[5])
        strong = [c for c in candidates if c[6] <= 45]
        if not strong:
            return None

        _, _, ref_cy, _, ref_ch, _, _ = strong[0]
        aligned_ids = [
            cid for (cid, cx, cy, cw, ch, _, _) in candidates
            if abs(cy - ref_cy) <= max(4.0, 0.35 * ref_ch)
            and (0.65 * ref_ch <= ch <= 1.45 * ref_ch)
        ]

        clean_mask = np.zeros_like(white_mask)
        for cid in aligned_ids:
            clean_mask[labels == cid] = 255

        ys, xs = np.where(clean_mask > 0)
        if len(ys) == 0:
            return None

        digit_box = clean_mask[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
        inv = 255 - digit_box  # Dígitos pretos em fundo branco
        up_digit = cv2.resize(inv, (0, 0), fx=2.0, fy=2.0, interpolation=cv2.INTER_CUBIC)
        dh, dw = up_digit.shape

        num_components = len(aligned_ids)
        copies = 3 if num_components == 1 else 2
        pad_x = 20
        gap = max(18, int(round(dh * 0.55)))
        canvas_h = max(dh, 24) + pad_x * 2
        canvas_w = pad_x * 2 + dw * copies + gap * (copies - 1)

        canvas = np.full((canvas_h, canvas_w), 255, dtype=np.uint8)
        y_top = (canvas_h - dh) // 2
        for c_idx in range(copies):
            x0 = pad_x + c_idx * (dw + gap)
            canvas[y_top:y_top + dh, x0:x0 + dw] = up_digit

        ocr_bgr = cv2.cvtColor(canvas, cv2.COLOR_GRAY2BGR)
        try:
            res = winocr.recognize_cv2_sync(ocr_bgr, self.ocr_lang)
        except Exception:
            return None

        raw_text = res.get("text", "").strip()
        if not raw_text:
            return None

        # Suporte caso o Albion mostre minutos (ex: '1m' ou '2m')
        min_match = re.search(r"(\d+)\s*m\b", raw_text, flags=re.IGNORECASE)
        if min_match:
            return int(min_match.group(1)) * 60

        # Normaliza confusões clássicas de OCR para dígitos
        after_norm = (
            raw_text.replace("O", "0")
            .replace("o", "0")
            .replace("I", "1")
            .replace("l", "1")
            .replace("|", "1")
            .replace("i", "1")
            .replace("!", "1")
            .replace("Z", "2")
            .replace("z", "2")
            .replace("S", "5")
            .replace("s", "5")
            .replace("B", "8")
        )

        if num_components == 1:
            for ch in after_norm:
                if ch.isdigit() and 1 <= int(ch) <= 9:
                    return int(ch)
            return None

        # Ignora decimais < 1s (ex: 0.7s) quando não há inteiro >= 1
        dec_match = re.search(r"(\d+)[.,](\d+)", after_norm)
        if dec_match:
            whole = int(dec_match.group(1))
            return whole if whole >= 1 else None

        nums = re.findall(r"\d+", after_norm)
        if nums:
            s = nums[0]
            if len(s) > num_components and num_components >= 2 and len(s) % num_components == 0:
                first_part = s[:num_components]
                if s == first_part * (len(s) // num_components):
                    s = first_part
            if s.startswith("0"):
                return None
            val = int(s)
            if 1 <= val <= 600:
                return val

        return None


# ==============================================================================
# Seletor Interativo da Barra (E | R | D | F | 1) com Divisórias ao Vivo
# ==============================================================================
class SkillBarSelector:
    def __init__(self, parent, callback):
        self.callback = callback
        self.win = tk.Toplevel(parent)
        self.win.title("Selecionar Barra de Skills (E até 1)")

        # Cobrir todos os monitores (Virtual Screen)
        user32 = ctypes.windll.user32
        vx = user32.GetSystemMetrics(76)  # SM_XVIRTUALSCREEN
        vy = user32.GetSystemMetrics(77)  # SM_YVIRTUALSCREEN
        vw = user32.GetSystemMetrics(78)  # SM_CXVIRTUALSCREEN
        vh = user32.GetSystemMetrics(79)  # SM_CYVIRTUALSCREEN
        if vw <= 0 or vh <= 0:
            vx, vy = 0, 0
            vw = self.win.winfo_screenwidth()
            vh = self.win.winfo_screenheight()

        self.vx = vx
        self.vy = vy
        self.win.overrideredirect(True)
        self.win.geometry(f"{vw}x{vh}+{vx}+{vy}")
        self.win.attributes("-alpha", 0.38)
        self.win.attributes("-topmost", True)
        self.win.config(bg="#111111")

        self.canvas = tk.Canvas(self.win, cursor="cross", bg="#111111", highlightthickness=0)
        self.canvas.pack(fill="both", expand=True)

        # Instrução no topo da tela principal
        self.canvas.create_text(
            abs(vx) + 40,
            abs(vy) + 40,
            anchor="nw",
            text="Arraste um retângulo cobrindo os 5 slots: do [E] até o [1]  (ESC para cancelar)",
            fill="#f2a900",
            font=("Segoe UI", 16, "bold"),
        )

        self.start_x = None
        self.start_y = None
        self.start_rx = None
        self.start_ry = None

        self.canvas.bind("<ButtonPress-1>", self.on_press)
        self.canvas.bind("<B1-Motion>", self.on_drag)
        self.canvas.bind("<ButtonRelease-1>", self.on_release)
        self.win.bind("<Escape>", lambda e: self.cancel())
        self.win.focus_force()

    def on_press(self, event):
        self.start_x = event.x
        self.start_y = event.y
        self.start_rx = event.x_root
        self.start_ry = event.y_root
        self.redraw_guides(event.x, event.y)

    def on_drag(self, event):
        if self.start_x is not None:
            self.redraw_guides(event.x, event.y)

    def redraw_guides(self, cur_x, cur_y):
        self.canvas.delete("guide")
        x1, x2 = sorted((self.start_x, cur_x))
        y1, y2 = sorted((self.start_y, cur_y))
        w = x2 - x1
        h = y2 - y1

        # Retângulo principal
        self.canvas.create_rectangle(x1, y1, x2, y2, outline="#f2a900", width=3, tags="guide")

        if w >= 50 and h >= 20:
            slot_w = w / 5.0
            for idx, slot in enumerate(SLOTS):
                sx1 = x1 + idx * slot_w
                sx2 = sx1 + slot_w
                if idx > 0:
                    self.canvas.create_line(
                        sx1, y1, sx1, y2, fill="#00FF66", width=2, dash=(4, 3), tags="guide"
                    )
                # Caixa central onde o número é lido
                cx1 = sx1 + slot_w * 0.16
                cx2 = sx1 + slot_w * 0.84
                cy1 = y1 + h * 0.24
                cy2 = y1 + h * 0.76
                self.canvas.create_rectangle(
                    cx1, cy1, cx2, cy2, outline="#00d4ff", width=1, tags="guide"
                )
                # Rótulo da skill acima do slot
                self.canvas.create_text(
                    (sx1 + sx2) / 2.0,
                    max(12, y1 - 14),
                    text=slot["label"],
                    fill="#ffffff",
                    font=("Segoe UI", 10, "bold"),
                    tags="guide",
                )

    def on_release(self, event):
        if self.start_rx is None:
            return
        rx1, rx2 = sorted((self.start_rx, event.x_root))
        ry1, ry2 = sorted((self.start_ry, event.y_root))
        self.win.destroy()
        if (rx2 - rx1) >= 60 and (ry2 - ry1) >= 20:
            self.callback([int(rx1), int(ry1), int(rx2), int(ry2)])
        else:
            self.callback(None)

    def cancel(self):
        self.win.destroy()
        self.callback(None)


# ==============================================================================
# Overlay Transparente da Party (Click-Through com F2, igual ao Easy CD)
# ==============================================================================
GWL_EXSTYLE = -20
WS_EX_LAYERED = 0x00080000
WS_EX_TRANSPARENT = 0x00000020
VK_F2 = 0x71


class PartyOverlayWindow:
    def __init__(self, parent, app_ref):
        self.app = app_ref
        self.win = tk.Toplevel(parent)
        self.win.title("Easy CD Overlay")
        self.win.overrideredirect(True)
        self.win.attributes("-topmost", True)
        # Cor chave de transparência no Windows
        self.transparent_key = "#010203"
        self.win.config(bg=self.transparent_key)
        self.win.attributes("-transparentcolor", self.transparent_key)
        self.win.attributes("-alpha", 0.88)

        pos = self.app.config.get("overlay_pos", [100, 100])
        self.win.geometry(f"+{int(pos[0])}+{int(pos[1])}")

        self.locked = False  # F2 alterna entre editável e click-through
        self.scale = float(self.app.config.get("overlay_scale", 1.0))
        self.cols = int(self.app.config.get("overlay_cols", 2))
        self.ordered_users = list(self.app.config.get("ordered_users", []))
        self.user_colors = dict(self.app.config.get("user_colors", {}))
        self.party_data = {}

        self._drag_x = 0
        self._drag_y = 0

        self.container = tk.Frame(self.win, bg=self.transparent_key)
        self.container.pack(padx=6, pady=6)

        # Barra de controles (fica oculta quando travado com F2)
        self.toolbar = tk.Frame(self.container, bg="#18181b", bd=1, relief="solid")
        self.toolbar.pack(fill="x", pady=(0, 6))

        drag_lbl = tk.Label(
            self.toolbar,
            text=" ✥ Arrastar (F2 Trava) ",
            bg="#18181b",
            fg="#f2a900",
            font=("Segoe UI", 8, "bold"),
            cursor="fleur",
        )
        drag_lbl.pack(side="left", padx=4, pady=3)
        drag_lbl.bind("<ButtonPress-1>", self._start_move)
        drag_lbl.bind("<B1-Motion>", self._on_move)

        tk.Button(
            self.toolbar, text="+", width=2, bg="#2563eb", fg="white", bd=0,
            font=("Segoe UI", 8, "bold"), command=lambda: self._change_scale(1.1)
        ).pack(side="left", padx=2, pady=2)
        tk.Button(
            self.toolbar, text="-", width=2, bg="#2563eb", fg="white", bd=0,
            font=("Segoe UI", 8, "bold"), command=lambda: self._change_scale(1 / 1.1)
        ).pack(side="left", padx=2, pady=2)
        self.grid_btn = tk.Button(
            self.toolbar, text=f"{self.cols} Cols", bg="#2563eb", fg="white", bd=0,
            font=("Segoe UI", 8, "bold"), padx=6, command=self._cycle_cols
        )
        self.grid_btn.pack(side="left", padx=2, pady=2)
        tk.Button(
            self.toolbar, text="✕", width=2, bg="#991b1b", fg="white", bd=0,
            font=("Segoe UI", 8, "bold"), command=self.close
        ).pack(side="right", padx=3, pady=2)

        self.cards_frame = tk.Frame(self.container, bg=self.transparent_key)
        self.cards_frame.pack()

        self.is_open = True
        self.render_cards()

    def _start_move(self, event):
        self._drag_x = event.x_root - self.win.winfo_x()
        self._drag_y = event.y_root - self.win.winfo_y()

    def _on_move(self, event):
        if self.locked:
            return
        nx = event.x_root - self._drag_x
        ny = event.y_root - self._drag_y
        self.win.geometry(f"+{nx}+{ny}")
        self.app.config["overlay_pos"] = [nx, ny]
        self.app.save_config()

    def _change_scale(self, factor):
        self.scale = max(0.65, min(2.2, self.scale * factor))
        self.app.config["overlay_scale"] = round(self.scale, 3)
        self.app.save_config()
        self.render_cards()

    def _cycle_cols(self):
        self.cols = 2 if self.cols >= 5 else self.cols + 1
        self.grid_btn.config(text=f"{self.cols} Cols")
        self.app.config["overlay_cols"] = self.cols
        self.app.save_config()
        self.render_cards()

    def move_user(self, idx, direction):
        if direction == -1 and idx > 0:
            self.ordered_users[idx - 1], self.ordered_users[idx] = (
                self.ordered_users[idx],
                self.ordered_users[idx - 1],
            )
        elif direction == 1 and idx < len(self.ordered_users) - 1:
            self.ordered_users[idx + 1], self.ordered_users[idx] = (
                self.ordered_users[idx],
                self.ordered_users[idx + 1],
            )
        self.app.config["ordered_users"] = self.ordered_users
        self.app.save_config()
        self.render_cards()

    def set_user_color(self, nickname, color_name):
        self.user_colors[nickname] = color_name
        self.app.config["user_colors"] = self.user_colors
        self.app.save_config()
        self.render_cards()

    def toggle_lock_clickthrough(self):
        if not self.is_open:
            return
        self.locked = not self.locked
        hwnd = ctypes.windll.user32.GetParent(self.win.winfo_id())
        if not hwnd:
            hwnd = self.win.winfo_id()
        ex_style = ctypes.windll.user32.GetWindowLongW(hwnd, GWL_EXSTYLE)
        if self.locked:
            ctypes.windll.user32.SetWindowLongW(
                hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED | WS_EX_TRANSPARENT
            )
            self.toolbar.pack_forget()
        else:
            ctypes.windll.user32.SetWindowLongW(
                hwnd, GWL_EXSTYLE, (ex_style | WS_EX_LAYERED) & ~WS_EX_TRANSPARENT
            )
            self.toolbar.pack(fill="x", pady=(0, 6), before=self.cards_frame)
        self.render_cards()

    def update_party_data(self, data: dict):
        if not self.is_open:
            return
        self.party_data = data or {}
        existing = [u for u in self.ordered_users if u in self.party_data]
        new_users = [u for u in self.party_data.keys() if u not in existing]
        self.ordered_users = existing + new_users
        self.app.config["ordered_users"] = self.ordered_users
        self.render_cards()

    def render_cards(self):
        if not self.is_open:
            return
        for child in self.cards_frame.winfo_children():
            child.destroy()

        if not self.ordered_users:
            lbl = tk.Label(
                self.cards_frame,
                text="Aguardando jogadores na Party...",
                bg="#1f2937",
                fg="#d1d5db",
                font=("Segoe UI", max(8, int(9 * self.scale))),
                padx=10,
                pady=6,
            )
            lbl.grid(row=0, column=0)
            return

        name_font = ("Segoe UI", max(7, int(9 * self.scale)), "bold")
        icon_font = ("Segoe UI Symbol", max(10, int(13 * self.scale)), "bold")
        pad_card_x = max(6, int(10 * self.scale))
        pad_card_y = max(4, int(6 * self.scale))

        for idx, nickname in enumerate(self.ordered_users):
            r = idx // self.cols
            c = idx % self.cols
            u_info = self.party_data.get(nickname, {})
            cds = u_info.get("cooldown", {}) if isinstance(u_info, dict) else {}
            color_label = self.user_colors.get(nickname, "Padrão")
            bg_hex = CARD_COLORS.get(color_label, CARD_COLORS["Padrão"])

            card = tk.Frame(
                self.cards_frame,
                bg=bg_hex,
                padx=pad_card_x,
                pady=pad_card_y,
                highlightbackground="#374151",
                highlightthickness=1,
            )
            card.grid(row=r, column=c, padx=4, pady=4, sticky="nsew")

            left = tk.Frame(card, bg=bg_hex)
            left.pack(side="left")

            tk.Label(left, text=nickname, bg=bg_hex, fg="#ffffff", font=name_font).pack(
                pady=(0, 2)
            )

            icons_row = tk.Frame(left, bg=bg_hex)
            icons_row.pack()

            # Apenas Verde (disponível) / Vermelho (em cooldown), sem números
            for field, symbol, _tooltip in OVERLAY_ICON_ORDER:
                in_cd = bool(cds.get(field, False))
                fg_color = "#ef4444" if in_cd else "#22c55e"
                tk.Label(
                    icons_row,
                    text=symbol,
                    bg=bg_hex,
                    fg=fg_color,
                    font=icon_font,
                    padx=2,
                ).pack(side="left")

            if not self.locked:
                ctrl_row = tk.Frame(left, bg=bg_hex)
                ctrl_row.pack(pady=(3, 0))
                var = tk.StringVar(value=color_label)
                opt = ttk.Combobox(
                    ctrl_row,
                    textvariable=var,
                    values=list(CARD_COLORS.keys()),
                    width=8,
                    state="readonly",
                    font=("Segoe UI", 7),
                )
                opt.pack(side="left")
                opt.bind(
                    "<<ComboboxSelected>>",
                    lambda e, n=nickname, v=var: self.set_user_color(n, v.get()),
                )

                arrows = tk.Frame(card, bg=bg_hex)
                arrows.pack(side="left", padx=(6, 0))
                tk.Button(
                    arrows, text="↑", bg=bg_hex, fg="#60a5fa", bd=0,
                    font=("Segoe UI", 8, "bold"), command=lambda i=idx: self.move_user(i, -1)
                ).pack()
                tk.Button(
                    arrows, text="↓", bg=bg_hex, fg="#60a5fa", bd=0,
                    font=("Segoe UI", 8, "bold"), command=lambda i=idx: self.move_user(i, 1)
                ).pack()

    def close(self):
        self.is_open = False
        self.app.on_overlay_closed()
        self.win.destroy()


# ==============================================================================
# Aplicação Principal do Cliente (Estilo Stalcraft)
# ==============================================================================
class EasyCDVisionApp:
    def __init__(self, root):
        self.root = root
        self.root.title("Easy CD - Albion Vision Tracker")
        self.root.geometry("520x650")
        self.root.resizable(False, False)

        self.is_tracking = False
        self.overlay_win = None
        self.cv_engine = AlbionCooldownCV()
        self.http = requests.Session()

        self.config = {
            "nickname": "",
            "party_id": "",
            "bbox": None,
            "overlay_pos": [100, 100],
            "overlay_scale": 1.0,
            "overlay_cols": 2,
            "ordered_users": [],
            "user_colors": {},
        }
        self.load_config()

        # Estado atual dos 5 cooldowns (booleanos enviados ao Firebase)
        self.slot_states = {s["field"]: False for s in SLOTS}
        self.slot_hit_streak = {s["field"]: 0 for s in SLOTS}
        self.slot_miss_streak = {s["field"]: 0 for s in SLOTS}
        self.slot_last_sec = {s["field"]: 0 for s in SLOTS}
        self.slot_last_time = {s["field"]: 0.0 for s in SLOTS}
        self.last_synced_states = None

        # Referências das imagens do preview no Tkinter
        self.preview_photos = [None] * 5
        self.preview_img_labels = []
        self.preview_status_labels = []

        self.setup_ui()
        self.log(f"Cliente inicializado. OCR Windows ativo ({self.cv_engine.ocr_lang}).")
        if self.config.get("bbox"):
            self.log(f"Zona salva carregada: {self.config['bbox']}")

        # Listener global da tecla F2 para travar/destravar o Overlay
        self._f2_was_down = False
        self.poll_f2_hotkey()

    def load_config(self):
        if os.path.exists(CONFIG_FILE):
            try:
                with open(CONFIG_FILE, "r", encoding="utf-8") as f:
                    self.config.update(json.load(f))
            except Exception as e:
                print(f"Erro ao carregar {CONFIG_FILE}: {e}")

    def save_config(self):
        try:
            with open(CONFIG_FILE, "w", encoding="utf-8") as f:
                json.dump(self.config, f, indent=2, ensure_ascii=False)
        except Exception as e:
            print(f"Erro ao salvar {CONFIG_FILE}: {e}")

    def setup_ui(self):
        main_frame = ttk.Frame(self.root, padding="14")
        main_frame.pack(fill="both", expand=True)

        title_label = tk.Label(
            main_frame,
            text="EASY CD • ALBION VISION",
            font=("Courier", 16, "bold"),
            bg="#121214",
            fg="#f2a900",
        )
        title_label.pack(pady=(0, 10))

        # 1. Configuração da Party
        cfg_frame = ttk.LabelFrame(main_frame, text=" Party & Jogador (Firebase) ", padding="10")
        cfg_frame.pack(fill="x", pady=4)

        tk.Label(cfg_frame, text="Nickname:").grid(row=0, column=0, sticky="w", pady=4)
        self.nickname_var = tk.StringVar(value=self.config.get("nickname", ""))
        ttk.Entry(cfg_frame, textvariable=self.nickname_var).grid(
            row=0, column=1, sticky="ew", padx=(10, 0), pady=4
        )

        tk.Label(cfg_frame, text="ID da Party:").grid(row=1, column=0, sticky="w", pady=4)
        self.party_var = tk.StringVar(value=self.config.get("party_id", ""))
        ttk.Entry(cfg_frame, textvariable=self.party_var).grid(
            row=1, column=1, sticky="ew", padx=(10, 0), pady=4
        )
        cfg_frame.columnconfigure(1, weight=1)

        # 2. Seleção da Zona da Barra (E até 1)
        region_frame = ttk.LabelFrame(
            main_frame, text=" Zona de Captura da Barra (E • R • D • F • 1) ", padding="10"
        )
        region_frame.pack(fill="x", pady=6)

        self.region_label = tk.Label(
            region_frame, text="Zona: Não selecionada", fg="#ef4444", bg="#121214", font=("Segoe UI", 9, "bold")
        )
        self.region_label.pack(side="left", padx=4)
        if self.config.get("bbox"):
            b = self.config["bbox"]
            self.region_label.config(
                text=f"Zona: ({b[0]}, {b[1]}) → ({b[2]}, {b[3]})", fg="#22c55e"
            )

        ttk.Button(
            region_frame, text="Selecionar na Tela", command=self.select_region
        ).pack(side="right", padx=4)

        # 3. Preview ao Vivo dos 5 Slots
        preview_frame = ttk.LabelFrame(
            main_frame, text=" Preview em Tempo Real (Divisão Automática) ", padding="8"
        )
        preview_frame.pack(fill="x", pady=6)

        slots_row = tk.Frame(preview_frame, bg="#121214")
        slots_row.pack(fill="x")

        for idx, s in enumerate(SLOTS):
            col = tk.Frame(slots_row, bg="#121214")
            col.pack(side="left", expand=True, fill="x", padx=3)

            tk.Label(
                col, text=s["label"], bg="#121214", fg="#e5e7eb", font=("Segoe UI", 8, "bold")
            ).pack()

            img_box = tk.Label(
                col,
                bg="#1e1e24",
                width=64,
                height=64,
                highlightbackground="#374151",
                highlightthickness=2,
            )
            img_box.pack(pady=3)
            # Imagem vazia inicial 64x64 para fixar tamanho em pixels
            blank = ImageTk.PhotoImage(Image.new("RGB", (64, 64), (30, 30, 36)))
            self.preview_photos[idx] = blank
            img_box.config(image=blank)
            self.preview_img_labels.append(img_box)

            st_lbl = tk.Label(
                col, text="AGUARDANDO", bg="#121214", fg="#9ca3af", font=("Consolas", 8, "bold")
            )
            st_lbl.pack()
            self.preview_status_labels.append(st_lbl)

        # 4. Botões de Ação
        btn_frame = ttk.Frame(main_frame)
        btn_frame.pack(fill="x", pady=(8, 4))

        self.start_btn = tk.Button(
            btn_frame,
            text="▶  INICIAR LEITURA EM TEMPO REAL",
            bg="#22c55e",
            fg="#ffffff",
            activebackground="#16a34a",
            activeforeground="#ffffff",
            font=("Segoe UI", 11, "bold"),
            height=2,
            bd=0,
            cursor="hand2",
            command=self.toggle_tracking,
        )
        self.start_btn.pack(fill="x", pady=(0, 6))

        self.overlay_btn = tk.Button(
            btn_frame,
            text="🖥  ABRIR OVERLAY DA PARTY (F2 Trava/Destrava)",
            bg="#2563eb",
            fg="#ffffff",
            activebackground="#1d4ed8",
            activeforeground="#ffffff",
            font=("Segoe UI", 10, "bold"),
            height=1,
            pady=6,
            bd=0,
            cursor="hand2",
            command=self.toggle_overlay,
        )
        self.overlay_btn.pack(fill="x")

        # 5. Console de Logs
        log_frame = ttk.LabelFrame(main_frame, text=" Console de Eventos ", padding="5")
        log_frame.pack(fill="both", expand=True, pady=(6, 0))

        self.log_text = tk.Text(
            log_frame,
            height=7,
            bg="#09090b",
            fg="#00FF66",
            font=("Consolas", 9),
            state="disabled",
            bd=0,
        )
        self.log_text.pack(fill="both", expand=True)

        # Se já tiver bbox salva, mostra uma captura inicial no preview
        if self.config.get("bbox"):
            self.root.after(250, self.refresh_static_preview)

    def log(self, msg):
        def _append():
            self.log_text.config(state="normal")
            self.log_text.insert(tk.END, f"[{time.strftime('%H:%M:%S')}] {msg}\n")
            self.log_text.see(tk.END)
            self.log_text.config(state="disabled")

        self.root.after(0, _append)

    def poll_f2_hotkey(self):
        try:
            state = ctypes.windll.user32.GetAsyncKeyState(VK_F2)
            is_down = (state & 0x8000) != 0
            if is_down and not self._f2_was_down:
                if self.overlay_win and self.overlay_win.is_open:
                    self.overlay_win.toggle_lock_clickthrough()
                    modo = "TRAVADO (Click-Through)" if self.overlay_win.locked else "DESTRAVADO (Editável)"
                    self.log(f"Overlay {modo} [F2]")
            self._f2_was_down = is_down
        except Exception:
            pass
        self.root.after(90, self.poll_f2_hotkey)

    def select_region(self):
        self.root.withdraw()
        self.root.update()
        time.sleep(0.25)

        def on_selected(bbox):
            self.root.deiconify()
            if bbox:
                self.config["bbox"] = bbox
                self.save_config()
                self.region_label.config(
                    text=f"Zona: ({bbox[0]}, {bbox[1]}) → ({bbox[2]}, {bbox[3]})",
                    fg="#22c55e",
                )
                self.log(f"Nova zona selecionada: {bbox}")
                self.refresh_static_preview()
            else:
                self.log("Seleção de zona cancelada.")

        SkillBarSelector(self.root, on_selected)

    def refresh_static_preview(self):
        bbox = self.config.get("bbox")
        if not bbox:
            return
        try:
            bar_bgr = grab_screen_region_bgr(bbox)
            if bar_bgr is None:
                return
            slots_bgr = self.split_bar_into_slots(bar_bgr)
            readings = [self.cv_engine.read_slot_seconds(s) for s in slots_bgr]
            states = [r is not None for r in readings]
            self.update_preview_widgets(slots_bgr, states, readings)
        except Exception as e:
            self.log(f"Aviso no preview inicial: {e}")

    @staticmethod
    def split_bar_into_slots(bar_bgr: np.ndarray):
        h, w = bar_bgr.shape[:2]
        slot_w = w / 5.0
        crops = []
        for i in range(5):
            x1 = int(round(i * slot_w))
            x2 = int(round((i + 1) * slot_w))
            crops.append(bar_bgr[:, x1:x2])
        return crops

    def update_preview_widgets(self, slots_bgr, states_bool, raw_seconds):
        for idx in range(5):
            crop = slots_bgr[idx]
            if crop is None or crop.size == 0:
                continue
            rgb = cv2.cvtColor(crop, cv2.COLOR_BGR2RGB)
            pil_img = Image.fromarray(rgb).resize((64, 64), Image.Resampling.BILINEAR)
            photo = ImageTk.PhotoImage(pil_img)
            self.preview_photos[idx] = photo

            in_cd = states_bool[idx]
            sec = raw_seconds[idx]
            border_color = "#ef4444" if in_cd else "#22c55e"
            self.preview_img_labels[idx].config(image=photo, highlightbackground=border_color)

            if in_cd:
                txt = f"CD ({sec}s)" if sec is not None else "EM CD"
                self.preview_status_labels[idx].config(text=txt, fg="#ef4444")
            else:
                self.preview_status_labels[idx].config(text="PRONTO", fg="#22c55e")

    def toggle_overlay(self):
        if self.overlay_win and self.overlay_win.is_open:
            self.overlay_win.close()
            return

        party_id = self.party_var.get().strip()
        if not party_id:
            messagebox.showwarning("ID da Party", "Preencha o ID da Party primeiro para abrir o Overlay.")
            return

        self.config["party_id"] = party_id
        self.config["nickname"] = self.nickname_var.get().strip()
        self.save_config()

        self.overlay_win = PartyOverlayWindow(self.root, self)
        self.overlay_btn.config(text="🖥  FECHAR OVERLAY DA PARTY", bg="#4b5563")
        self.log("Overlay da Party aberto. Pressione F2 para travar/clicar através.")
        threading.Thread(target=self.overlay_sync_loop, daemon=True).start()

    def on_overlay_closed(self):
        self.overlay_win = None
        self.overlay_btn.config(
            text="🖥  ABRIR OVERLAY DA PARTY (F2 Trava/Destrava)", bg="#2563eb"
        )
        self.log("Overlay fechado.")

    def overlay_sync_loop(self):
        """Busca o estado dos jogadores da party no Firebase para alimentar o Overlay integrado."""
        while self.overlay_win and self.overlay_win.is_open:
            party_id = self.config.get("party_id", "").strip()
            if party_id:
                try:
                    url = f"{FIREBASE_DB_URL}/parties/{party_id}/users.json"
                    r = self.http.get(url, timeout=2.5)
                    if r.status_code == 200:
                        data = r.json() or {}
                        self.root.after(
                            0,
                            lambda d=data: self.overlay_win.update_party_data(d)
                            if (self.overlay_win and self.overlay_win.is_open)
                            else None,
                        )
                except Exception:
                    pass
            time.sleep(0.45)

    def register_user_in_firebase(self, party_id: str, nickname: str):
        url = f"{FIREBASE_DB_URL}/parties/{party_id}/users/{nickname}.json"
        payload = {
            "nickname": nickname,
            "cooldown": {s["field"]: False for s in SLOTS},
        }
        r = self.http.patch(url, json=payload, timeout=3.0)
        r.raise_for_status()

    def push_cooldowns_to_firebase(self, party_id: str, nickname: str, changed_dict: dict):
        url = f"{FIREBASE_DB_URL}/parties/{party_id}/users/{nickname}/cooldown.json"
        r = self.http.patch(url, json=changed_dict, timeout=2.0)
        r.raise_for_status()

    def toggle_tracking(self):
        if self.is_tracking:
            self.is_tracking = False
            self.start_btn.config(text="▶  INICIAR LEITURA EM TEMPO REAL", bg="#22c55e")
            self.log("Monitoramento pausado.")
            return

        nickname = self.nickname_var.get().strip()
        party_id = self.party_var.get().strip()
        bbox = self.config.get("bbox")

        if not nickname:
            messagebox.showerror("Erro", "Digite seu Nickname.")
            return
        if not party_id or len(party_id) > 10:
            messagebox.showerror("Erro", "Digite um ID da Party válido (máx. 10 caracteres).")
            return
        if not bbox:
            messagebox.showerror("Erro", "Selecione a zona da barra de skills (do E até o 1) na tela.")
            return

        self.config["nickname"] = nickname
        self.config["party_id"] = party_id
        self.save_config()

        # Reinicia estados locais
        for s in SLOTS:
            f = s["field"]
            self.slot_states[f] = False
            self.slot_hit_streak[f] = 0
            self.slot_miss_streak[f] = 0
            self.slot_last_sec[f] = 0
            self.slot_last_time[f] = 0.0
        self.last_synced_states = None

        self.is_tracking = True
        self.start_btn.config(text="⏹  PARAR MONITORAMENTO", bg="#ef4444")
        self.log(f"Iniciando leitura para '{nickname}' na Party '{party_id}'...")

        threading.Thread(target=self.tracking_loop, daemon=True).start()

    def tracking_loop(self):
        party_id = self.config["party_id"]
        nickname = self.config["nickname"]

        try:
            self.register_user_in_firebase(party_id, nickname)
            self.last_synced_states = dict(self.slot_states)
            self.log("Conectado ao Firebase com sucesso!")
        except Exception as e:
            self.log(f"Erro ao conectar no Firebase: {str(e)[:45]}")

        # Intervalo rápido (~14 leituras por segundo)
        scan_interval = 0.07

        while self.is_tracking:
            t0 = time.perf_counter()
            try:
                bbox = self.config.get("bbox")
                bar_bgr = grab_screen_region_bgr(bbox)
                if bar_bgr is not None:
                    slots_bgr = self.split_bar_into_slots(bar_bgr)
                    raw_seconds = []
                    changed_payload = {}

                    for idx, s in enumerate(SLOTS):
                        field = s["field"]
                        sec = self.cv_engine.read_slot_seconds(slots_bgr[idx])
                        raw_seconds.append(sec)

                        currently_in_cd = self.slot_states[field]
                        now_t = time.perf_counter()

                        if sec is not None:
                            self.slot_miss_streak[field] = 0
                            self.slot_hit_streak[field] += 1
                            self.slot_last_sec[field] = sec
                            self.slot_last_time[field] = now_t
                            if not currently_in_cd:
                                if sec >= 2 or self.slot_hit_streak[field] >= 2:
                                    self.slot_states[field] = True
                                    changed_payload[field] = True
                                    self.log(f"[{s['key']}] {s['label']} entrou em recarga ({sec}s)")
                        else:
                            self.slot_hit_streak[field] = 0
                            if currently_in_cd:
                                self.slot_miss_streak[field] += 1
                                effective_rem = self.slot_last_sec[field] - (now_t - self.slot_last_time[field])
                                min_misses = 2 if effective_rem <= 2.2 else 20
                                if self.slot_miss_streak[field] >= min_misses:
                                    self.slot_states[field] = False
                                    self.slot_miss_streak[field] = 0
                                    self.slot_last_sec[field] = 0
                                    changed_payload[field] = False
                                    self.log(f"[{s['key']}] {s['label']} disponível!")

                    states_list = [self.slot_states[s["field"]] for s in SLOTS]
                    self.root.after(
                        0,
                        lambda sb=slots_bgr, st=states_list, rs=raw_seconds: self.update_preview_widgets(
                            sb, st, rs
                        ),
                    )

                    # Envia ao Firebase APENAS quando há mudança real de estado (True <-> False)
                    if changed_payload:
                        try:
                            self.push_cooldowns_to_firebase(party_id, nickname, changed_payload)
                            self.last_synced_states = dict(self.slot_states)
                        except Exception as net_err:
                            self.log(f"Falha ao atualizar Firebase: {str(net_err)[:40]}")

            except Exception as loop_err:
                self.log(f"Erro no loop de visão: {str(loop_err)[:45]}")

            elapsed = time.perf_counter() - t0
            time.sleep(max(0.05, scan_interval - elapsed))


if __name__ == "__main__":
    root = tk.Tk()
    style = ttk.Style()
    style.theme_use("clam")

    style.configure("TFrame", background="#121214")
    style.configure("TLabelFrame", background="#121214", foreground="#f2a900")
    style.configure("TLabelFrame.Label", background="#121214", foreground="#f2a900", font=("Segoe UI", 9, "bold"))
    style.configure("TLabel", background="#121214", foreground="#e6e6fa")
    style.configure("TEntry", fieldbackground="#09090b", foreground="#00FF66", insertcolor="#00FF66")
    style.configure("TButton", background="#f2a900", foreground="#000000", font=("Segoe UI", 9, "bold"))

    root.config(bg="#121214")
    app = EasyCDVisionApp(root)
    root.mainloop()
