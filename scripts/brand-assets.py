#!/usr/bin/env python3
"""Render every AVA1 logo image from the SVG masters in branding/.

The masters are SVG (branding/*.svg): the app icon, the bare mark and the name lockups. The name
is set in Titan One (SIL Open Font License, branding/fonts/TitanOne-OFL.txt) and kept as
outlines, so nothing here needs the font installed. This writes branding/png/:

  icon-1024.png, icon-dark-1024.png   the app icon on white and on black
  icon-256.png                        the logo docs.rs shows (`html_logo_url` in crates/ava1)
  favicon-32.png                      the docs.rs favicon (`html_favicon_url`)
  mark-1024.png, mark-white-1024.png  the mark alone, transparent
  lockup-*.png                        the name lockups; the README shows the horizontal ones
  github-social-preview.png           1280x640, for the repository's social preview

Needs rsvg-convert (brew install librsvg) and Pillow.
Run it again whenever a master changes:  python3 scripts/brand-assets.py
"""
import subprocess
import tempfile
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
B = ROOT / "branding"
PNG = B / "png"
BLACK = (11, 11, 12, 255)


def render(svg: Path, out: Path, *, w: int | None = None, h: int | None = None) -> Path:
    """Render an SVG file to a PNG at the given width or height."""
    args = ["rsvg-convert", str(svg), "-o", str(out)]
    if w:
        args += ["-w", str(w)]
    if h:
        args += ["-h", str(h)]
    subprocess.run(args, check=True)
    return out


def on(bg: tuple, size: tuple[int, int], art: Image.Image, scale: float) -> Image.Image:
    """`art` centred on a plain `bg` canvas, its width fitted to `scale` of the canvas width."""
    canvas = Image.new("RGBA", size, bg)
    k = size[0] * scale / art.width
    fitted = art.resize((round(art.width * k), round(art.height * k)), Image.Resampling.LANCZOS)
    canvas.alpha_composite(fitted, ((size[0] - fitted.width) // 2, (size[1] - fitted.height) // 2))
    return canvas


def optimise(p: Path) -> None:
    Image.open(p).save(p, optimize=True)


def main() -> None:
    PNG.mkdir(parents=True, exist_ok=True)
    jobs = [
        ("icon.svg", "icon-1024.png", 1024),
        ("icon-dark.svg", "icon-dark-1024.png", 1024),
        ("icon.svg", "icon-256.png", 256),
        ("icon.svg", "favicon-32.png", 32),
        ("mark.svg", "mark-1024.png", 1024),
        ("mark-white.svg", "mark-white-1024.png", 1024),
    ]
    for src, dst, w in jobs:
        optimise(render(B / src, PNG / dst, w=w))
    for kind in ("horizontal", "stacked"):
        for c in ("black", "white"):
            optimise(render(B / f"lockup-{kind}-{c}.svg", PNG / f"lockup-{kind}-{c}.png", h=400))
    with tempfile.TemporaryDirectory() as tmp:
        art = Image.open(render(B / "lockup-horizontal-white.svg", Path(tmp) / "l.png", w=1800)).convert("RGBA")
        on(BLACK, (1280, 640), art, 0.66).save(PNG / "github-social-preview.png", optimize=True)
    for p in sorted(PNG.iterdir()):
        print(f"{p.relative_to(ROOT)}  {Image.open(p).size[0]}x{Image.open(p).size[1]}")


if __name__ == "__main__":
    main()
