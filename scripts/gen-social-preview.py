#!/usr/bin/env python3
"""Regenerate docs/images/social-preview.png from docs/images/social-preview.svg.

One-off maintainer tool, not wired into CI. It rasterises the SVG with a
headless Chromium browser (Edge or Chrome), which shapes the Arabic text and
renders the gradients exactly as GitHub's viewers do — no Python imaging
dependencies. Run it after `cargo run -p duja-app --example gen_exe_icon`
whenever the brand mark or the social card changes:

    python scripts/gen-social-preview.py

The output is the 1280x640 card GitHub expects; upload it manually in
Settings -> Social preview after merging.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SVG = os.path.join(REPO, "docs", "images", "social-preview.svg")
PNG = os.path.join(REPO, "docs", "images", "social-preview.png")

BROWSERS = [
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
]


def find_browser() -> str:
    for path in BROWSERS:
        if os.path.isfile(path):
            return path
    for name in ("msedge", "chrome", "chromium"):
        found = shutil.which(name)
        if found:
            return found
    sys.exit("no Chromium-based browser found; install Edge or Chrome")


def main() -> None:
    if not os.path.isfile(SVG):
        sys.exit(f"missing {SVG}")
    browser = find_browser()
    subprocess.run(
        [
            browser,
            "--headless=new",
            "--disable-gpu",
            f"--screenshot={PNG}",
            "--window-size=1280,640",
            "file:///" + SVG.replace(os.sep, "/"),
        ],
        check=True,
        capture_output=True,
    )
    size = os.path.getsize(PNG)
    print(f"wrote {PNG} ({size} bytes)")


if __name__ == "__main__":
    main()
