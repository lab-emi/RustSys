#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Validate repository-relative links in the README and source documentation."""

from __future__ import annotations

import argparse
from pathlib import Path
import sys
from urllib.parse import unquote, urlsplit

import markdown

from check_site import Document, LinkParser
from foundation_paths import relocated


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--max-pages", type=int, default=1000)
    parser.add_argument("--max-total-bytes", type=int, default=128 * 1024 * 1024)
    args = parser.parse_args()
    if args.max_pages <= 0 or args.max_total_bytes <= 0:
        parser.error("all budgets must be positive")
    root = args.root.resolve(strict=True)
    documents: dict[Path, Document] = {}
    total_bytes = 0

    def read_document(path: Path) -> Document:
        nonlocal total_bytes
        if path in documents:
            return documents[path]
        if len(documents) >= args.max_pages:
            raise ValueError("source page budget exceeded")
        size = path.stat().st_size
        total_bytes += size
        if size > 8 * 1024 * 1024 or total_bytes > args.max_total_bytes:
            raise ValueError("source byte budget exceeded")
        html = markdown.markdown(path.read_text(encoding="utf-8"), extensions=[
            "toc", "tables", "attr_list", "md_in_html", "fenced_code",
            "admonition", "pymdownx.superfences", "pymdownx.details", "pymdownx.tabbed",
        ])
        reader = LinkParser(20000)
        reader.feed(html)
        documents[path] = reader.document
        return reader.document

    try:
        read_document(root / "README.md")
        for path in (root / "docs").rglob("*.md"):
            read_document(path.resolve())
        # Snapshot the bounded source set: destination Markdown is loaded lazily.
        sources = tuple(documents.items())
        checked = 0
        failures = 0
        for source, document in sources:
            for link in document.links:
                url = urlsplit(link)
                if url.scheme or url.netloc:
                    continue
                path = unquote(url.path)
                target = (source.parent / path).resolve() if path else source
                checked += 1
                moved_target = False
                if target.is_relative_to(root) and not target.exists():
                    moved = relocated(root, target)
                    if moved is not None:
                        target = moved[0]
                        moved_target = True
                allowed = target.is_relative_to(root) or moved_target
                if not allowed or not target.exists():
                    print(f"{source.relative_to(root)}: missing source target: {link}")
                    failures += 1
                    continue
                if url.fragment and target.suffix.lower() == ".md":
                    destination = read_document(target)
                    if unquote(url.fragment) not in destination.ids:
                        print(f"{source.relative_to(root)}: missing source fragment: {link}")
                        failures += 1
        print(f"markdown_check: {len(sources)} pages; {checked} local references; {failures} failures")
        return 1 if failures else 0
    except (OSError, ValueError) as error:
        print(f"markdown_check: budget_or_input_error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
