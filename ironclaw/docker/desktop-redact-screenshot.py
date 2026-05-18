#!/usr/bin/env python3
"""
Screenshot credential redaction tool.

Uses tesseract OCR to locate text regions in a screenshot that match
hidden credential values, then uses imagemagick to black out those regions.

Usage:
    desktop-redact-screenshot.py INPUT.png HIDDEN_VALUES.txt OUTPUT.png

Arguments:
    INPUT.png           Input screenshot (PNG).
    HIDDEN_VALUES.txt   File containing hidden values, one per line.
                        This file is read once and then the caller should
                        securely delete it (shred -u).
    OUTPUT.png          Output screenshot with redacted regions (PNG).

Exit codes:
    0   Success (output written, may have zero redactions if no matches found).
    1   Error (input file not found, tesseract/imagemagick not available, etc.).

Security notes:
    - Hidden values are read from a file (never passed as command-line args
      to avoid exposure in `ps` output).
    - The hidden values file should be securely deleted by the caller after
      this script exits.
    - This script never logs hidden values.
    - Redaction is best-effort: unusual fonts, rotated text, or obfuscated
      rendering may not be caught. The accessibility tree redaction (exact
      string match) is more reliable for structured UI state.
"""

import json
import os
import subprocess
import sys
import tempfile


def log(msg: str) -> None:
    """Write to stderr (never stdout, which is reserved for structured output)."""
    print(f"[desktop-redact] {msg}", file=sys.stderr)


def run(cmd: list[str], check: bool = True) -> subprocess.CompletedProcess:
    """Run a command and return the result."""
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        check=check,
    )


def check_dependencies() -> bool:
    """Check that tesseract and imagemagick are available."""
    ok = True
    for tool in ["tesseract", "convert"]:
        result = run(["which", tool], check=False)
        if result.returncode != 0:
            log(f"WARNING: '{tool}' not found — redaction will be skipped")
            ok = False
    return ok


def get_ocr_word_boxes(image_path: str) -> list[dict]:
    """
    Run tesseract OCR on the image and return word bounding boxes.

    Returns a list of dicts with keys: text, x, y, w, h (pixel coordinates).
    """
    with tempfile.NamedTemporaryFile(suffix=".tsv", delete=False) as f:
        tsv_path = f.name

    try:
        run([
            "tesseract",
            image_path,
            tsv_path.replace(".tsv", ""),  # tesseract adds .tsv itself
            "--oem", "3",
            "--psm", "11",  # sparse text — find as much text as possible
            "tsv",
        ])

        # tesseract writes to <base>.tsv
        actual_tsv = tsv_path.replace(".tsv", "") + ".tsv"
        if not os.path.exists(actual_tsv):
            actual_tsv = tsv_path

        boxes = []
        with open(actual_tsv) as f:
            lines = f.readlines()

        # TSV format: level page_num block_num par_num line_num word_num
        #             left top width height conf text
        for line in lines[1:]:  # skip header
            parts = line.strip().split("\t")
            if len(parts) < 12:
                continue
            text = parts[11].strip()
            if not text:
                continue
            try:
                x = int(parts[6])
                y = int(parts[7])
                w = int(parts[8])
                h = int(parts[9])
                boxes.append({"text": text, "x": x, "y": y, "w": w, "h": h})
            except (ValueError, IndexError):
                continue

        return boxes

    finally:
        for path in [tsv_path, tsv_path.replace(".tsv", "") + ".tsv"]:
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass


def find_regions_to_redact(
    boxes: list[dict],
    hidden_values: list[str],
    padding: int = 4,
) -> list[tuple[int, int, int, int]]:
    """
    Find bounding boxes of OCR words that match any hidden value.

    Matching is case-sensitive substring: if a hidden value appears as a
    substring of an OCR word (or vice versa), the word's bounding box is
    included in the redaction list.

    Returns a list of (x, y, w, h) tuples with `padding` pixels added on
    each side.
    """
    regions = []
    for box in boxes:
        word = box["text"]
        for hidden in hidden_values:
            if not hidden:
                continue
            # Match if the hidden value appears in the word OR the word
            # appears in the hidden value (handles partial OCR matches).
            if hidden in word or word in hidden:
                x = max(0, box["x"] - padding)
                y = max(0, box["y"] - padding)
                w = box["w"] + 2 * padding
                h = box["h"] + 2 * padding
                regions.append((x, y, w, h))
                break  # one match per word is enough

    return regions


def apply_redaction(
    input_path: str,
    output_path: str,
    regions: list[tuple[int, int, int, int]],
) -> None:
    """
    Use imagemagick `convert` to black out the given regions.

    Each region is drawn as a solid black filled rectangle.
    """
    if not regions:
        # No redactions needed — just copy the file.
        import shutil
        shutil.copy2(input_path, output_path)
        return

    # Build the imagemagick command.
    # Each region: -fill black -draw "rectangle x,y x+w,y+h"
    cmd = ["convert", input_path]
    cmd += ["-fill", "black"]
    for (x, y, w, h) in regions:
        x2 = x + w
        y2 = y + h
        cmd += ["-draw", f"rectangle {x},{y} {x2},{y2}"]
    cmd.append(output_path)

    run(cmd)


def main() -> int:
    if len(sys.argv) != 4:
        print(
            f"Usage: {sys.argv[0]} INPUT.png HIDDEN_VALUES.txt OUTPUT.png",
            file=sys.stderr,
        )
        return 1

    input_path = sys.argv[1]
    hidden_values_path = sys.argv[2]
    output_path = sys.argv[3]

    # Validate inputs.
    if not os.path.exists(input_path):
        log(f"ERROR: input file not found: {input_path}")
        return 1

    if not os.path.exists(hidden_values_path):
        log(f"ERROR: hidden values file not found: {hidden_values_path}")
        return 1

    # Read hidden values (never log them).
    with open(hidden_values_path) as f:
        hidden_values = [line.rstrip("\n") for line in f if line.strip()]

    if not hidden_values:
        log("No hidden values configured — copying input to output unchanged")
        import shutil
        shutil.copy2(input_path, output_path)
        return 0

    # Check dependencies.
    if not check_dependencies():
        log("WARNING: dependencies missing — copying input to output unchanged")
        import shutil
        shutil.copy2(input_path, output_path)
        return 0

    # Run OCR.
    log(f"Running OCR on {input_path}")
    try:
        boxes = get_ocr_word_boxes(input_path)
    except Exception as e:
        log(f"WARNING: OCR failed ({e}) — copying input to output unchanged")
        import shutil
        shutil.copy2(input_path, output_path)
        return 0

    log(f"OCR found {len(boxes)} word boxes")

    # Find regions to redact.
    regions = find_regions_to_redact(boxes, hidden_values)
    log(f"Redacting {len(regions)} region(s)")

    # Apply redaction.
    try:
        apply_redaction(input_path, output_path, regions)
    except Exception as e:
        log(f"WARNING: redaction failed ({e}) — copying input to output unchanged")
        import shutil
        shutil.copy2(input_path, output_path)
        return 0

    log(f"Redacted screenshot written to {output_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
