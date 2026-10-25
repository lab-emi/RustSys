# SPDX-License-Identifier: Apache-2.0
# SPDX-FileCopyrightText: 2026 RustSys contributors
"""Keep relocated canonical-source links valid on the documentation site."""
from pathlib import Path
import re
from urllib.parse import unquote, urlsplit
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from foundation_paths import relocated

def on_page_markdown(markdown, *, page, config, files):
    root = Path(config.docs_dir).resolve().parent
    source = Path(page.file.abs_src_path)
    def resolve(match):
        url = urlsplit(match.group(1))
        if url.scheme or url.netloc or not url.path:
            return match.group(0)
        target = (source.parent / unquote(url.path)).resolve()
        if target.exists():
            return match.group(0)
        moved = relocated(root, target)
        if moved is None:
            return match.group(0)
        destination = moved[1] + ("#" + url.fragment if url.fragment else "")
        return f"]({destination})"
    parts = re.split(r"(```.*?```|~~~.*?~~~)", markdown, flags=re.DOTALL)
    for i in range(0, len(parts), 2):
        parts[i] = re.sub(r"\]\(([^\s)]+)\)", resolve, parts[i])
    return "".join(parts)
