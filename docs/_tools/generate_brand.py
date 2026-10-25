#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""The canonical family renderer lives in the RustEDA superproject."""
from pathlib import Path
import runpy

root = Path(__file__).resolve().parents[4]
renderer = root / "scripts/generate_brand.py"
if not renderer.is_file():
    raise SystemExit("Use the recursive RustEDA checkout and run scripts/generate_brand.py")
runpy.run_path(str(renderer), run_name="__main__")
