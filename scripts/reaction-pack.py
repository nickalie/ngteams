#!/usr/bin/env python3
"""Build the reaction pack the app draws reactions with: crates/teams-app/assets/reactions/catalog.tsv and
crates/teams-ui/assets/reactions/emoji.bin.

- The catalog is every reaction Teams offers, in its picker's categories and order: the web client's emoticon
  metadata (a public, unauthenticated read of Teams' CDN, `personal-expressions/v1/metadata/<version>/default.json`,
  the version the web bundle's config names as `emoticonAssetVersion`). Only the id (the reaction key Teams sends),
  the code points, whether it takes a skin tone, and the short description are kept, in a TSV of our own.
- The pictures are Microsoft's Fluent Emoji 3D (MIT, https://github.com/microsoft/fluentui-emoji), the art Teams
  draws its reactions in, matched to the catalog by code points (variation selectors ignored), scaled to SIZE px and
  packed as WebP into one file; a catalog line names its picture's offset and length there. A reaction no Fluent
  picture matches draws its code points as text.

Usage: scripts/reaction-pack.py <fluentui-emoji checkout> [<metadata.json>]
       (a sparse checkout of `assets/*/metadata.json`, `assets/*/3D/*` and `assets/*/Default/3D/*` is enough; without
       a metadata file the script reads the CDN's for VERSION)
"""

import io
import json
import pathlib
import subprocess
import sys
import urllib.request

from PIL import Image

VERSION = "0f52465a47bf42f299c74a639443f33e"
METADATA = (
    "https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v1/metadata/" + VERSION + "/default.json"
)
SIZE = 64
QUALITY = 88
IGNORED = {0xFE0E, 0xFE0F}

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/teams-ui/assets/reactions"
CATALOG_OUT = ROOT / "crates/teams-app/assets/reactions"


def sequence(points):
    """Code points as the lookup key: lower-case hex, variation selectors dropped."""
    return " ".join(f"{p:x}" for p in points if p not in IGNORED)


def fluent_pictures(checkout):
    """The Fluent 3D picture of each code point sequence (the default skin tone's for one that takes a tone)."""
    found = {}
    for meta_path in sorted((checkout / "assets").glob("*/metadata.json")):
        meta = json.loads(meta_path.read_text())
        points = [int(p, 16) for p in meta["unicode"].split()]
        folder = meta_path.parent
        pictures = sorted((folder / "3D").glob("*.png")) or sorted((folder / "Default/3D").glob("*.png"))
        if pictures:
            found.setdefault(sequence(points), pictures[0])
    return found


def parts(points, pictures):
    """The Fluent pictures a sequence no single picture matches is made of (Teams' fist bump is two emoji), longest
    first from the left; `None` when some part matches none."""
    found = []
    at = 0
    while at < len(points):
        for end in range(len(points), at, -1):
            picture = pictures.get(sequence(points[at:end]))
            if picture is not None:
                found.append(picture)
                at = end
                break
        else:
            if points[at] in IGNORED:
                at += 1
                continue
            return None
    return found or None


def compose(paths):
    """Several pictures side by side in one square, overlapping as much as they must."""
    canvas = Image.new("RGBA", (SIZE, SIZE))
    side = SIZE if len(paths) == 1 else round(SIZE * 0.62)
    step = 0 if len(paths) == 1 else (SIZE - side) / (len(paths) - 1)
    for index, path in enumerate(paths):
        part = Image.open(path).convert("RGBA").resize((side, side), Image.LANCZOS)
        canvas.alpha_composite(part, (round(index * step), (SIZE - side) // 2))
    return canvas


def webp(paths):
    picture = compose(paths)
    out = io.BytesIO()
    picture.save(out, "WEBP", quality=QUALITY, method=6)
    return out.getvalue()


def main():
    checkout = pathlib.Path(sys.argv[1])
    if len(sys.argv) > 2:
        metadata = json.loads(pathlib.Path(sys.argv[2]).read_text())
    else:
        with urllib.request.urlopen(METADATA) as answer:
            metadata = json.load(answer)
    commit = subprocess.run(
        ["git", "-C", str(checkout), "rev-parse", "HEAD"], capture_output=True, text=True, check=True
    ).stdout.strip()
    pictures = fluent_pictures(checkout)
    packed = bytearray()
    placed = {}
    lines = [
        "# The reactions Teams offers, by its picker's categories, in its order (scripts/reaction-pack.py).",
        f"# Catalog: Teams' emoticon metadata {VERSION}. Pictures: Fluent Emoji 3D (MIT), {commit}, {SIZE} px WebP.",
        "# `@<category>`, then per reaction: id, code points, `d` when it takes a skin tone, offset and length of its",
        "# picture in emoji.bin (empty: none), description.",
    ]
    seen = set()
    missing = []
    for category in metadata["categories"]:
        lines.append("@" + category["title"])
        for emoticon in category["emoticons"]:
            if emoticon["id"] in seen:
                continue
            seen.add(emoticon["id"])
            points = [ord(c) for c in emoticon["unicode"]]
            key = sequence(points)
            picture = pictures.get(key)
            picture = (picture,) if picture is not None else parts(points, pictures)
            picture = tuple(picture) if picture is not None else None
            offset = length = ""
            if picture is not None:
                if picture not in placed:
                    data = webp(picture)
                    placed[picture] = (len(packed), len(data))
                    packed.extend(data)
                offset, length = placed[picture]
            else:
                missing.append(emoticon["id"])
            fields = [
                emoticon["id"],
                " ".join(f"{p:x}" for p in points),
                "d" if emoticon.get("diverse") else "",
                str(offset),
                str(length),
                emoticon["description"].replace("\t", " ").strip(),
            ]
            lines.append("\t".join(fields))
    OUT.mkdir(parents=True, exist_ok=True)
    CATALOG_OUT.mkdir(parents=True, exist_ok=True)
    (CATALOG_OUT / "catalog.tsv").write_text("\n".join(lines) + "\n")
    (OUT / "emoji.bin").write_bytes(bytes(packed))
    print(f"{len(seen)} reactions, {len(placed)} pictures, {len(packed)} bytes; no picture for {len(missing)}")
    if missing:
        print("  " + " ".join(missing))


if __name__ == "__main__":
    main()
