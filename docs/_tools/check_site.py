#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Check local links in bounded static HTML output without contacting the network."""

from __future__ import annotations

import argparse
from dataclasses import dataclass, field
from html.parser import HTMLParser
from pathlib import Path
import sys
from urllib.parse import unquote, urlsplit


@dataclass
class Document:
    ids: set[str] = field(default_factory=set)
    links: list[str] = field(default_factory=list)


class LinkParser(HTMLParser):
    def __init__(self, max_links: int) -> None:
        super().__init__(convert_charrefs=True)
        self.document = Document()
        self.max_links = max_links

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        for key, value in attrs:
            if not value:
                continue
            if key == "id":
                self.document.ids.add(value)
            if key in {"href", "src", "poster"}:
                if len(self.document.links) >= self.max_links:
                    raise ValueError(f"per-page link budget exceeded ({self.max_links})")
                self.document.links.append(value)
            if key == "srcset":
                for candidate in value.split(","):
                    if len(self.document.links) >= self.max_links:
                        raise ValueError(f"per-page link budget exceeded ({self.max_links})")
                    self.document.links.append(candidate.strip().split()[0])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("site", type=Path)
    parser.add_argument("--site-prefix", default="/RustSys/")
    parser.add_argument("--max-pages", type=int, default=1000)
    parser.add_argument("--max-page-bytes", type=int, default=8 * 1024 * 1024)
    parser.add_argument("--max-total-bytes", type=int, default=128 * 1024 * 1024)
    parser.add_argument("--max-links-per-page", type=int, default=20000)
    args = parser.parse_args()
    for field_name in ("max_pages", "max_page_bytes", "max_total_bytes", "max_links_per_page"):
        if getattr(args, field_name) <= 0:
            parser.error(f"--{field_name.replace('_', '-')} must be positive")
    root = args.site.resolve(strict=True)
    documents: dict[Path, Document] = {}
    total_bytes = 0
    try:
        for path in root.rglob("*.html"):
            if len(documents) >= args.max_pages:
                raise ValueError(f"page budget exceeded ({args.max_pages})")
            page_bytes = path.stat().st_size
            total_bytes += page_bytes
            if page_bytes > args.max_page_bytes or total_bytes > args.max_total_bytes:
                raise ValueError("HTML input byte budget exceeded")
            reader = LinkParser(args.max_links_per_page)
            reader.feed(path.read_text(encoding="utf-8"))
            documents[path] = reader.document
    except (ValueError, OSError) as error:
        print(f"docs_check: budget_or_input_error: {error}", file=sys.stderr)
        return 2
    if not documents:
        print("docs_check: input_error: no HTML pages found", file=sys.stderr)
        return 2

    failures = 0
    checked = 0
    for source, document in documents.items():
        for link in document.links:
            parsed = urlsplit(link)
            if parsed.scheme or parsed.netloc:
                continue
            url_path = unquote(parsed.path)
            if url_path.startswith("/"):
                if not url_path.startswith(args.site_prefix):
                    print(f"{source.relative_to(root)}: outside site prefix: {link}")
                    failures += 1
                    continue
                target = root / url_path[len(args.site_prefix):]
            else:
                target = source.parent / url_path if url_path else source
            target = target.resolve()
            if not target.is_relative_to(root):
                print(f"{source.relative_to(root)}: leaves site directory: {link}")
                failures += 1
                continue
            if target.is_dir():
                target = target / "index.html"
            checked += 1
            if not target.is_file():
                print(f"{source.relative_to(root)}: missing target: {link}")
                failures += 1
            elif parsed.fragment and target in documents:
                fragment = unquote(parsed.fragment)
                if fragment not in documents[target].ids:
                    print(f"{source.relative_to(root)}: missing fragment: {link}")
                    failures += 1
    print(f"docs_check: {len(documents)} pages; {checked} local references; {failures} failures")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
