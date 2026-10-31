"""Draws the call backgrounds the app ships (crates/teams-media/assets/backgrounds/*.jpg): our own artwork in the
spirit of Teams' stock backgrounds (a room, landscapes, abstract light), drawn from code so nothing of Microsoft's is
in the repo.

Usage: python3 tools/backgrounds/make.py crates/teams-media/assets/backgrounds   (numpy and Pillow)
"""

import math
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

W, H = 1280, 720
RNG = np.random.default_rng(20261031)


def vertical(top, bottom, height=H, width=W):
    t = np.linspace(0.0, 1.0, height)[:, None, None]
    top, bottom = np.array(top, float), np.array(bottom, float)
    return np.broadcast_to(top + (bottom - top) * t, (height, width, 3)).copy()


def to_image(array):
    return Image.fromarray(np.clip(array, 0, 255).astype(np.uint8))


def ridge(base, amplitude, seed, octaves=4):
    rng = np.random.default_rng(seed)
    x = np.linspace(0, 1, W)
    y = np.full(W, base, float)
    for octave in range(octaves):
        freq = 2 ** octave * rng.uniform(1.5, 2.5)
        phase = rng.uniform(0, 2 * math.pi)
        y += amplitude / (octave + 1) ** 1.3 * np.sin(2 * math.pi * freq * x + phase)
    return y


def fill_below(array, line, colour, fade=None):
    rows = np.arange(H)[:, None]
    below = rows >= line[None, :]
    colour = np.array(colour, float)
    if fade is not None:
        depth = np.clip((rows - line[None, :]) / fade[0], 0, 1)[..., None]
        shade = colour + (np.array(fade[1], float) - colour) * depth
        array[below] = shade[below]
    else:
        array[below] = colour


def office():
    img = to_image(vertical((236, 230, 222), (214, 206, 196)))
    d = ImageDraw.Draw(img)
    # the window, its sky and the town beyond
    wx0, wy0, wx1, wy1 = 90, 90, 600, 520
    sky = to_image(vertical((150, 196, 236), (214, 232, 246), wy1 - wy0, wx1 - wx0))
    sd = ImageDraw.Draw(sky)
    rng = np.random.default_rng(3)
    x = 0
    while x < wx1 - wx0:
        bw = int(rng.integers(40, 90))
        bh = int(rng.integers(90, 260))
        shade = int(rng.integers(170, 205))
        sd.rectangle([x, wy1 - wy0 - bh, x + bw, wy1 - wy0], fill=(shade - 10, shade, shade + 12))
        x += bw + int(rng.integers(4, 16))
    img.paste(sky, (wx0, wy0))
    frame = (250, 248, 244)
    d.rectangle([wx0 - 14, wy0 - 14, wx1 + 14, wy0], fill=frame)
    d.rectangle([wx0 - 14, wy1, wx1 + 14, wy1 + 22], fill=frame)
    d.rectangle([wx0 - 14, wy0 - 14, wx0, wy1 + 22], fill=frame)
    d.rectangle([wx1, wy0 - 14, wx1 + 14, wy1 + 22], fill=frame)
    d.rectangle([(wx0 + wx1) // 2 - 6, wy0, (wx0 + wx1) // 2 + 6, wy1], fill=frame)
    # the floor and a low cabinet
    d.rectangle([0, 600, W, H], fill=(176, 142, 108))
    d.rectangle([700, 470, 1240, 600], fill=(250, 248, 244))
    d.rectangle([700, 462, 1240, 474], fill=(196, 160, 122))
    for i in range(3):
        d.rectangle([716 + i * 176, 490, 876 + i * 176, 586], outline=(226, 222, 214), width=3)
    # shelves of books
    for shelf_y in (190, 330):
        d.rectangle([720, shelf_y, 1220, shelf_y + 12], fill=(196, 160, 122))
        bx = 740
        palette = [(92, 98, 168), (214, 120, 92), (98, 150, 132), (232, 196, 110), (120, 110, 150), (190, 90, 96)]
        while bx < 1180:
            bw = int(rng.integers(14, 30))
            bh = int(rng.integers(70, 112))
            colour = palette[int(rng.integers(0, len(palette)))]
            if rng.random() < 0.15:
                bx += 30
                continue
            d.rectangle([bx, shelf_y - bh, bx + bw, shelf_y], fill=colour)
            bx += bw + 3
    # a plant in a pot
    for leaf in range(26):
        angle = rng.uniform(-2.6, -0.5)
        length = rng.uniform(70, 150)
        cx, cy = 1120, 470
        tip = (cx + math.cos(angle) * length, cy + math.sin(angle) * length)
        d.line([cx, cy, tip[0], tip[1]], fill=(70, 130, 84), width=5)
        d.ellipse([tip[0] - 16, tip[1] - 9, tip[0] + 16, tip[1] + 9], fill=(84, 150, 96))
    d.polygon([(1080, 470), (1160, 470), (1150, 540), (1090, 540)], fill=(226, 214, 200))
    # a pendant lamp
    d.line([980, 0, 980, 80], fill=(80, 80, 80), width=3)
    d.pieslice([930, 60, 1030, 160], 180, 360, fill=(60, 64, 72))
    return img.filter(ImageFilter.GaussianBlur(2.2))


def mountains():
    a = vertical((246, 196, 160), (252, 232, 214))
    yy, xx = np.mgrid[0:H, 0:W]
    glow = np.exp(-(((xx - 880) / 260.0) ** 2 + ((yy - 300) / 180.0) ** 2))[..., None]
    a += glow * np.array([12, 6, -20])
    sun = ((xx - 880) ** 2 + (yy - 300) ** 2) < 70 ** 2
    a[sun] = (255, 242, 214)
    layers = [
        (330, 60, (196, 150, 160), 11),
        (400, 55, (160, 112, 140), 12),
        (470, 45, (118, 82, 120), 13),
        (550, 35, (78, 56, 92), 14),
        (630, 25, (50, 38, 66), 15),
    ]
    for base, amp, colour, seed in layers:
        fill_below(a, ridge(base, amp, seed), colour)
    return to_image(a).filter(ImageFilter.GaussianBlur(1.2))


def city_lights():
    a = vertical((14, 18, 44), (40, 26, 64))
    yy, xx = np.mgrid[0:H, 0:W].astype(float)
    # the street's glow low in the picture
    a += np.exp(-((yy - H) / 260.0) ** 2)[..., None] * np.array([60, 34, 30])
    colours = [(255, 190, 110), (255, 140, 96), (120, 170, 255), (220, 130, 220), (255, 226, 150)]
    light = np.zeros_like(a)
    for _ in range(90):
        r = float(RNG.uniform(10, 54))
        x, y = float(RNG.uniform(0, W)), float(RNG.uniform(H * 0.3, H))
        c = np.array(colours[int(RNG.integers(0, len(colours)))], float)
        k = float(RNG.uniform(0.18, 0.5))
        dist = np.sqrt((xx - x) ** 2 + (yy - y) ** 2)
        disc = np.clip((r - dist) / 3.0, 0, 1) * (0.75 + 0.25 * np.clip(dist / r, 0, 1))
        light += disc[..., None] * c * k
    return to_image(a + light).filter(ImageFilter.GaussianBlur(2.5))


def waves():
    yy, xx = np.mgrid[0:H, 0:W].astype(float)
    a = vertical((70, 64, 160), (150, 120, 220))
    for i, (colour, phase, base) in enumerate([
        ((98, 100, 196), 0.0, 0.30),
        ((123, 131, 235), 1.3, 0.45),
        ((164, 140, 240), 2.1, 0.60),
        ((200, 170, 245), 3.0, 0.75),
        ((232, 214, 250), 4.2, 0.90),
    ]):
        line = H * base + 70 * np.sin(xx[0] / W * 2 * math.pi * (1.0 + 0.3 * i) + phase) \
            + 30 * np.sin(xx[0] / W * 6 * math.pi + phase * 2)
        rows = yy >= line[None, :]
        depth = np.clip((yy - line[None, :]) / 260.0, 0, 1)[..., None]
        shade = np.array(colour, float) * (1 - 0.25 * depth)
        a = np.where(rows[..., None], shade, a)
    return to_image(a).filter(ImageFilter.GaussianBlur(3))


def seaside():
    a = vertical((122, 186, 236), (206, 230, 246))
    yy, xx = np.mgrid[0:H, 0:W].astype(float)
    horizon = 400
    sea = vertical((40, 120, 170), (90, 170, 200))
    a[horizon:] = sea[horizon:]
    # soft swells across the water
    swell = (np.sin(yy / 7.0 + np.sin(xx / 90.0) * 2.0) > 0.85) & (yy > horizon + 8)
    a[swell] = a[swell] * 0.85 + 255 * 0.15
    shore = ridge(560, 18, 21, 3)
    fill_below(a, shore, (236, 214, 176), fade=(160, (214, 188, 146)))
    foam = (yy >= shore[None, :] - 6) & (yy < shore[None, :])
    a[foam] = (246, 248, 250)
    img = to_image(a)
    d = ImageDraw.Draw(img)
    for cx, cy, s in ((260, 150, 1.0), (900, 110, 1.4), (1120, 230, 0.8)):
        for dx, dy, r in ((0, 0, 40), (45, -14, 52), (95, 0, 40), (48, 12, 44)):
            d.ellipse([cx + dx * s - r * s, cy + dy * s - r * s, cx + dx * s + r * s, cy + dy * s + r * s],
                      fill=(250, 252, 255))
    return img.filter(ImageFilter.GaussianBlur(1.6))


def forest():
    a = vertical((206, 222, 214), (232, 238, 228))
    img = to_image(a)
    rng = np.random.default_rng(7)
    for depth, colour in enumerate([(170, 196, 182), (128, 164, 146), (88, 128, 108), (52, 88, 70), (30, 58, 46)]):
        layer = Image.new('RGBA', (W, H), (0, 0, 0, 0))
        d = ImageDraw.Draw(layer)
        count = 22 - depth * 3
        base = 380 + depth * 75
        for _ in range(count):
            x = float(rng.uniform(-40, W + 40))
            height = float(rng.uniform(200, 340)) * (0.7 + depth * 0.18)
            width = height * 0.34
            top = base - height
            d.polygon([(x, top), (x - width / 2, base + 2), (x + width / 2, base + 2)], fill=colour + (255,))
        d.rectangle([0, base, W, H], fill=colour + (255,))
        img = Image.alpha_composite(img.convert('RGBA'), layer.filter(ImageFilter.GaussianBlur(4 - depth * 0.7)))
        fog = Image.new('RGBA', (W, H), (230, 238, 232, 40))
        img = Image.alpha_composite(img, fog)
    return img.convert('RGB').filter(ImageFilter.GaussianBlur(1.0))


BACKGROUNDS = {
    'office': office,
    'mountains': mountains,
    'city-lights': city_lights,
    'waves': waves,
    'seaside': seaside,
    'forest': forest,
}

if __name__ == '__main__':
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for name, draw in BACKGROUNDS.items():
        draw().save(out / f'{name}.jpg', quality=86, optimize=True, progressive=False)
        print(name)
