#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Render the original RustEDA signal-forge identity and bounded GIF animation.

Documentation-only dependencies: docs/brand/requirements.txt. No Rust runtime uses
this generator. All frames come from native SVG geometry defined in this file.
"""

from __future__ import annotations

import argparse
import hashlib
import html
import io
import json
from pathlib import Path

import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "docs" / "assets"
INK, PANEL, BORDER = "#101923", "#17242F", "#304451"
COPPER, TEAL, PAPER, MUTED = "#EF8B52", "#57D6BE", "#F4F1E9", "#A1B0BA"
WIDTH, HEIGHT, FRAMES, DURATION_MS = 1200, 400, 84, 100
MAX_PIXELS = 1200 * 400
MAX_FRAMES = 96
MAX_GIF_BYTES = 1024 * 1024


def text(x: float, y: float, value: str, size: int = 16, fill: str = PAPER,
         weight: int = 400, mono: bool = False, **attrs: object) -> str:
    extra = " ".join(f'{key.replace("_", "-")}="{value}"' for key, value in attrs.items())
    family = "DejaVu Sans Mono,monospace" if mono else "DejaVu Sans,sans-serif"
    return (f'<text x="{x}" y="{y}" fill="{fill}" font-family="{family}" xml:space="preserve" '
            f'font-size="{size}" font-weight="{weight}" {extra}>{html.escape(value)}</text>')


def svg(width: int, height: int, body: str, title: str, description: str) -> str:
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
            f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="title desc">'
            f'<title id="title">{html.escape(title)}</title>'
            f'<desc id="desc">{html.escape(description)}</desc>{body}</svg>\n')


def mark(tool: str = "rustsys") -> str:
    """A shared terminal/chip frame surrounding a tool-specific signal glyph."""
    body = f'<rect width="256" height="256" rx="54" fill="{INK}"/>'
    body += (f'<g fill="none" stroke-width="12" stroke-linecap="round" stroke-linejoin="round">'
             f'<path d="M79 50H50V79M50 177V206H79" stroke="{COPPER}"/>'
             f'<path d="M177 50H206V79M206 177V206H177" stroke="{TEAL}"/>')
    if tool == "rustsys":
        body += (f'<path d="M69 108H106M69 148H106" stroke="{COPPER}"/>'
                 f'<path d="M108 89H130C180 89 180 167 130 167H108Z" stroke="{PAPER}"/>'
                 f'<path d="M168 128H189" stroke="{TEAL}"/>')
    else:
        body += (f'<path d="M69 102H104L152 154H188" stroke="{COPPER}"/>'
                 f'<path d="M69 154H104L152 102H188" stroke="{TEAL}"/>')
    body += '</g>'
    if tool != "rustsys":
        body += f'<circle cx="128" cy="128" r="12" fill="{PAPER}"/>'
    else:
        body += f'<circle cx="69" cy="108" r="7" fill="{COPPER}"/>'
        body += f'<circle cx="189" cy="128" r="7" fill="{TEAL}"/>'
    return body


def wordmark(tool: str, subtitle: str) -> str:
    body = f'<rect width="760" height="176" rx="28" fill="{INK}"/>'
    body += f'<g transform="translate(20 20) scale(.53125)">{mark(tool.lower())}</g>'
    body += text(180, 89, tool, 58, weight=700)
    body += text(184, 127, subtitle, 19, MUTED)
    return svg(760, 176, body, f"{tool} wordmark", subtitle)


def wire(path: str, active: bool = False, copper: bool = False, width: int = 2) -> str:
    return (f'<path d="{path}" fill="none" stroke="'
            f'{COPPER if active and copper else TEAL if active else BORDER}" '
            f'stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round"/>')


def pulse(x1: float, y1: float, x2: float, y2: float, progress: float, color: str) -> str:
    if not 0 <= progress <= 1:
        return ""
    x, y = x1 + (x2 - x1) * progress, y1 + (y2 - y1) * progress
    return (f'<circle cx="{x:.2f}" cy="{y:.2f}" r="8" fill="{color}" opacity=".12"/>'
            f'<circle cx="{x:.2f}" cy="{y:.2f}" r="3" fill="{color}"/>')


def banner(frame: int) -> str:
    """An 8.4-second loop: read RTL, reduce logic, map gates, propagate a pulse."""
    t = frame / FRAMES
    phase = min(3, int(t * 4))
    active_labels = ["READ RTL", "OPTIMIZE", "MAP CELLS", "WRITE NETLIST"]
    body = f'<rect width="{WIDTH}" height="{HEIGHT}" rx="24" fill="{INK}"/>'
    # A very quiet circuit-board grid; no photographic or external assets.
    body += '<g opacity=".16">'
    for x in range(24, WIDTH, 24):
        for y in range(96, 336, 24):
            body += f'<circle cx="{x}" cy="{y}" r=".8" fill="{MUTED}"/>'
    body += '</g>'
    body += f'<g transform="translate(32 20) scale(.21)">{mark()}</g>'
    body += text(98, 57, "RustSys", 29, weight=700)
    body += text(1164, 43, "VERILOG  →  GATE-LEVEL NETLIST", 13, MUTED,
                 weight=700, text_anchor="end", letter_spacing="1")
    body += text(1164, 66, "CLI first. Ready for agents.", 14, TEAL, text_anchor="end")

    cards = [(32, 326, "01", "RTL / VERILOG"),
             (438, 278, "02", "SYNTHESIZE"),
             (796, 372, "03", "GATE-LEVEL NETLIST")]
    for x, width, number, label in cards:
        body += (f'<rect x="{x}" y="100" width="{width}" height="218" rx="14" '
                 f'fill="{PANEL}" stroke="{BORDER}"/>')
        body += text(x + 18, 126, number, 11, COPPER, mono=True)
        body += text(x + 49, 126, label, 12, MUTED, weight=700, letter_spacing=".8")

    # Actual Verilog: a clock-enabled eight-bit AND result register.
    code = [("module enabled_and (", PAPER),
            ("  input clk, en,", MUTED),
            ("  input [7:0] a, b,", MUTED),
            ("  output reg [7:0] q", MUTED),
            (");", PAPER),
            ("  always @(posedge clk)", COPPER),
            ("    if (en) q <= a & b;", TEAL),
            ("endmodule", PAPER)]
    revealed = min(len(code), 1 + int(t * 33))
    for i, (line, color) in enumerate(code):
        opacity = "1" if i < revealed else ".18"
        body += text(49, 153 + i * 20, line, 13, color, mono=True, opacity=opacity)
    if t < .21:
        row = max(0, revealed - 1)
        body += f'<rect x="{51 + min(30, len(code[row][0])) * 7.83}" y="{142 + row * 20}" width="6" height="14" fill="{TEAL}"/>'

    # Flow arrows are animated, while the pipeline stays understandable in frame 0.
    for start, end, offset in [(367, 428, .05), (725, 786, .33)]:
        body += wire(f'M{start} 207H{end - 8}', active=True, copper=start < 500)
        body += f'<path d="M{end - 14} 201L{end - 6} 207L{end - 14} 213" fill="none" stroke="{COPPER if start < 500 else TEAL}" stroke-width="2"/>'
        body += pulse(start, 207, end - 10, 207, ((t - offset) * 4) % 1,
                      COPPER if start < 500 else TEAL)

    # Bounded optimization graph. A few nodes settle into a compact gate cone.
    nodes = [(469, 174), (469, 236), (508, 148), (508, 205),
             (508, 269), (555, 174), (555, 236), (605, 205), (654, 205)]
    edges = [(0, 3), (1, 3), (2, 5), (3, 5), (3, 6), (4, 6), (5, 7), (6, 7), (7, 8)]
    energized = t >= .22
    for i, (a, b) in enumerate(edges):
        xa, ya = nodes[a]
        xb, yb = nodes[b]
        body += wire(f'M{xa} {ya}L{xb} {yb}', energized and (i + frame // 4) % 3 != 0)
    # The two peripheral branches visibly dissolve during the optimize phase.
    redundant_opacity = .17 if t >= .38 else .8
    for i, (x, y) in enumerate(nodes):
        fade = redundant_opacity if i in (2, 4) else 1
        fill = TEAL if energized and (i + frame // 3) % 4 == 0 else PANEL
        body += f'<circle cx="{x}" cy="{y}" r="7" fill="{fill}" stroke="{TEAL if energized else BORDER}" stroke-width="2" opacity="{fade}"/>'
    body += text(577, 296, active_labels[phase], 11, TEAL, weight=700,
                 text_anchor="middle", letter_spacing="1.5")

    # Exact schematic: a,b -> AND -> MUX -> DFF, with q feedback and en select.
    active = t >= .51
    body += text(815, 170, "a", 11, MUTED, mono=True)
    body += text(815, 213, "b", 11, MUTED, mono=True)
    body += text(815, 145, "en", 10, MUTED, mono=True)
    body += text(815, 264, "clk", 11, MUTED, mono=True)
    body += wire('M840 166H865M840 208H865', active, copper=True)
    body += (f'<path d="M866 153H884C915 153 915 221 884 221H866Z" '
             f'fill="{INK}" stroke="{COPPER if active else MUTED}" stroke-width="2"/>')
    body += text(879, 193, "&", 17, PAPER, mono=True)
    body += text(884, 242, "AND × 8", 10, MUTED, mono=True, text_anchor="middle")
    body += wire('M908 187H922', active)
    body += f'<path d="M922 151L947 163V220L922 232Z" fill="{INK}" stroke="{TEAL if active else MUTED}" stroke-width="2"/>'
    body += text(930, 199, "M", 10, MUTED, mono=True)
    body += wire('M840 141H934V149', active)
    body += wire('M947 193H990M840 259H976V231H990', active)
    body += f'<rect x="991" y="170" width="72" height="76" rx="5" fill="{INK}" stroke="{TEAL if active else MUTED}" stroke-width="2"/>'
    body += text(1001, 199, "D", 12, PAPER, mono=True)
    body += text(1041, 199, "Q", 12, PAPER, mono=True)
    body += f'<path d="M992 223L1002 231L992 239" fill="none" stroke="{MUTED}" stroke-width="1.5"/>'
    body += text(1027, 158, "DFF × 8", 10, MUTED, mono=True, text_anchor="middle")
    body += wire('M1063 194H1138M1110 194V282H912V213H922', active)
    body += f'<circle cx="1110" cy="194" r="3" fill="{TEAL if active else BORDER}"/>'
    body += text(1143, 199, "q", 11, TEAL, mono=True)
    body += text(980, 304, "Liberty cell library", 10, MUTED, text_anchor="middle")
    if active:
        body += pulse(1065, 194, 1137, 194, ((t - .51) * 6) % 1, TEAL)
        body += pulse(841, 259, 974, 259, ((t - .51) * 6) % 1, COPPER)
        if (frame // 3) % 2 == 0:
            body += '<rect x="1005" y="215" width="41" height="4" rx="2" fill="' + TEAL + '" opacity=".7"/>'

    body += f'<rect x="32" y="336" width="1136" height="40" rx="9" fill="#0B1219" stroke="{BORDER}"/>'
    body += text(48, 361, "$", 14, TEAL, mono=True)
    body += text(68, 361, "rustsys synth --top enabled_and --lib cells.lib --out gates.v enabled_and.v", 13, PAPER, mono=True)
    body += text(1151, 361, "RTL → gates", 12, COPPER, mono=True, text_anchor="end")
    return svg(WIDTH, HEIGHT, body, "RustSys: Verilog to gate-level netlist",
               "A clock-enabled eight-bit AND in Verilog maps to an AND, feedback multiplexer and flip-flop bank. "
               "Animated pulses describe data flow; this is an illustration, not a run or proof result.")


def raster(source: str) -> Image.Image:
    png = cairosvg.svg2png(bytestring=source.encode(), output_width=WIDTH,
                          output_height=HEIGHT)
    return Image.open(io.BytesIO(png)).convert("RGB")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--static-only", action="store_true", help="Skip rendering the GIF and contact sheet")
    args = parser.parse_args()
    if WIDTH * HEIGHT > MAX_PIXELS or FRAMES > MAX_FRAMES:
        raise SystemExit("ASSET_BUDGET_EXCEEDED: configured dimensions or frames exceed hard caps")
    ASSETS.mkdir(parents=True, exist_ok=True)
    entries = {}
    for name, source in {
        "rusteda-logo.svg": svg(256, 256, mark("rusteda"), "RustEDA signal-forge mark",
                                "A shared terminal and chip frame surrounding crossing copper and teal signals."),
        "rustsys-logo.svg": svg(256, 256, mark(), "RustSys synthesis mark",
                                "Copper input signals enter a logic gate and emerge as a teal output signal."),
        "favicon.svg": svg(256, 256, mark(), "RustSys", "RustSys logic synthesis mark."),
        "rustsys-wordmark.svg": wordmark("RustSys", "From Verilog to standard cells."),
        "rusteda-wordmark.svg": wordmark("RustEDA", "Tools that speak CLI."),
        "rustsys-banner.svg": banner(65),
    }.items():
        (ASSETS / name).write_text(source)
        entries[name] = {"bytes": len(source.encode()), "sha256": hashlib.sha256(source.encode()).hexdigest()}
    if not args.static_only:
        # 84 indexed frames occupy 38.5 MiB. One RGB frame is 1.4 MiB; Cairo,
        # quantization scratch and the contact sheet keep estimated RSS < 256 MiB.
        frames = []
        samples = []
        palette = raster(banner(65)).quantize(colors=96, method=Image.Quantize.MEDIANCUT)
        for frame in range(FRAMES):
            image = raster(banner(frame))
            frames.append(image.quantize(palette=palette, dither=Image.Dither.NONE))
            if frame in (0, 21, 42, 65):
                samples.append(image.resize((600, 200)))
        output = ASSETS / "rustsys-banner.gif"
        frames[0].save(output, save_all=True, append_images=frames[1:], duration=DURATION_MS,
                       loop=0, optimize=True, disposal=1)
        if output.stat().st_size > MAX_GIF_BYTES:
            raise SystemExit("ASSET_BUDGET_EXCEEDED: GIF exceeds 1 MiB; reduce frame count or palette")
        # QA output is kept in .work rather than distributed with the docs.
        qa = ROOT / ".work" / "brand"
        qa.mkdir(parents=True, exist_ok=True)
        contact = Image.new("RGB", (1200, 400), INK)
        for index, sample in enumerate(samples):
            contact.paste(sample, ((index % 2) * 600, (index // 2) * 200))
        contact.save(qa / "rustsys-banner-contact-sheet.png")
        raster(banner(65)).save(qa / "rustsys-banner-poster.png")
        with Image.open(output) as gif:
            entries[output.name] = {
                "bytes": output.stat().st_size,
                "sha256": hashlib.sha256(output.read_bytes()).hexdigest(),
                "width": gif.width, "height": gif.height, "frames": gif.n_frames,
                "duration_ms": sum((gif.seek(i), gif.info.get("duration", 0))[1]
                                   for i in range(gif.n_frames)),
                "loop": gif.info.get("loop"),
            }
        (qa / "assets-manifest.json").write_text(json.dumps(entries, indent=2) + "\n")
    print(json.dumps(entries, indent=2))


if __name__ == "__main__":
    main()
