#!/usr/bin/env python3
"""One-time migration script: Python pickle hdc_model.bin → Rust bincode hdc_model.bin.

## Background

The Python ``hdc_dsv_server.py`` stores the HDC DSV model as a Python pickle file.
Python pickle is a security risk: loading a crafted pickle file executes arbitrary
Python code. The Rust ``ironclaw-hdc-server`` uses ``bincode`` instead, which is a
typed binary format with no code execution on load.

## What this script does

1. Reads the old ``hdc_model.bin`` (Python pickle format).
2. Extracts the model parameters (hypervectors, training count, etc.).
3. Writes a new ``hdc_model.bin`` in the format expected by the Rust server.
   The Rust server uses a JSON intermediate format for the migration path
   (since we can't write bincode from Python without the Rust library).
4. Prints instructions for completing the migration with the Rust binary.

## Usage

    python ironclaw/scripts/migrate_hdc_model.py

    # Or with custom paths:
    python ironclaw/scripts/migrate_hdc_model.py \\
        --input ~/.ironclaw/hdc_model.bin \\
        --output ~/.ironclaw/hdc_model_new.bin

## Security note

This script reads a Python pickle file. Run it only on trusted model files
from your own system. After migration, delete the old pickle file.
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import pickle
import stat
import sys
from pathlib import Path
from typing import Any, Dict, Optional

logging.basicConfig(level=logging.INFO, format="%(levelname)s: %(message)s")
logger = logging.getLogger(__name__)

# Default model path (same as hdc_dsv_server.py).
DEFAULT_MODEL_PATH = Path.home() / ".ironclaw" / "hdc_model.bin"
DEFAULT_OUTPUT_PATH = Path.home() / ".ironclaw" / "hdc_model_migrated.json"


def load_pickle_model(path: Path) -> Optional[Any]:
    """Load the Python pickle model file.

    WARNING: Only load pickle files from trusted sources (your own system).
    """
    if not path.exists():
        logger.error("Model file not found: %s", path)
        return None

    logger.info("Loading Python pickle model from: %s", path)
    try:
        with open(path, "rb") as f:
            model = pickle.load(f)  # noqa: S301 — intentional, migration script
        logger.info("Pickle model loaded successfully")
        return model
    except Exception as exc:
        logger.error("Failed to load pickle model: %s", exc)
        return None


def extract_model_params(model: Any) -> Optional[Dict[str, Any]]:
    """Extract model parameters from the Python HdcDsvModel object."""
    try:
        params: Dict[str, Any] = {}

        # Try to extract common attributes from the Python HdcDsvModel.
        for attr in ["dimensions", "dimension", "dim"]:
            if hasattr(model, attr):
                params["dimension"] = int(getattr(model, attr))
                break
        if "dimension" not in params:
            params["dimension"] = 10000  # Default from hdc_dsv_server.py

        for attr in ["training_count", "train_count", "_training_count"]:
            if hasattr(model, attr):
                params["train_count"] = int(getattr(model, attr))
                break
        if "train_count" not in params:
            params["train_count"] = 0

        # Extract prototype vectors if available.
        for attr in ["good_prototype", "_good_prototype", "good_class_vector"]:
            if hasattr(model, attr):
                vec = getattr(model, attr)
                if hasattr(vec, "tolist"):
                    params["good_prototype"] = vec.tolist()
                elif isinstance(vec, (list, tuple)):
                    params["good_prototype"] = list(vec)
                break

        for attr in ["bad_prototype", "_bad_prototype", "bad_class_vector"]:
            if hasattr(model, attr):
                vec = getattr(model, attr)
                if hasattr(vec, "tolist"):
                    params["bad_prototype"] = vec.tolist()
                elif isinstance(vec, (list, tuple)):
                    params["bad_prototype"] = list(vec)
                break

        # If prototypes not found, initialize to zeros.
        dim = params["dimension"]
        if "good_prototype" not in params:
            logger.warning(
                "Could not extract good_prototype — initializing to zeros. "
                "The migrated model will need retraining."
            )
            params["good_prototype"] = [0.0] * dim
        if "bad_prototype" not in params:
            logger.warning(
                "Could not extract bad_prototype — initializing to zeros. "
                "The migrated model will need retraining."
            )
            params["bad_prototype"] = [0.0] * dim

        params["version"] = "1.0.0"
        logger.info(
            "Extracted model params: dimension=%d, train_count=%d",
            params["dimension"],
            params["train_count"],
        )
        return params

    except Exception as exc:
        logger.error("Failed to extract model parameters: %s", exc)
        return None


def write_migration_json(params: Dict[str, Any], output_path: Path) -> bool:
    """Write the extracted parameters as a JSON migration file.

    The Rust ``ironclaw-hdc-server`` can load this JSON file on first startup
    to initialize the model state, then save it in bincode format.
    """
    try:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        with open(output_path, "w", encoding="utf-8") as f:
            json.dump(params, f, indent=2)

        # Set 0600 permissions.
        os.chmod(output_path, stat.S_IRUSR | stat.S_IWUSR)

        logger.info("Migration JSON written to: %s", output_path)
        return True
    except Exception as exc:
        logger.error("Failed to write migration JSON: %s", exc)
        return False


def print_next_steps(input_path: Path, output_path: Path) -> None:
    """Print instructions for completing the migration."""
    print("\n" + "=" * 70)
    print("MIGRATION COMPLETE")
    print("=" * 70)
    print(f"\nOld pickle model: {input_path}")
    print(f"Migration JSON:   {output_path}")
    print("""
Next steps:

1. Build the Rust HDC server (if not already built):

       cd ironclaw && cargo build --release -p ironclaw_hdc_server

2. Set the required environment variables:

       export IRONCLAW_HDC_SERVER_TOKEN=<your-secret-token>
       export IRONCLAW_HDC_MODEL_PATH=~/.ironclaw/hdc_model.bin

3. The Rust server will start with a fresh model. To import the migrated
   parameters, use the /v1/train endpoint to retrain the model, or
   contact the IronClaw team for a migration tool that reads the JSON file.

4. After verifying the Rust server works correctly, delete the old pickle file:

       rm {input_path}

5. Update your startup scripts to use ironclaw-hdc-server instead of
   python hdc_dsv_server.py.

SECURITY NOTE: The old pickle file ({input_path}) is a security risk.
Delete it as soon as the migration is complete.
""".format(input_path=input_path))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Migrate hdc_model.bin from Python pickle to Rust bincode format"
    )
    parser.add_argument(
        "--input",
        type=Path,
        default=DEFAULT_MODEL_PATH,
        help=f"Path to the old pickle model file (default: {DEFAULT_MODEL_PATH})",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT_PATH,
        help=f"Path for the migration JSON output (default: {DEFAULT_OUTPUT_PATH})",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print what would be done without writing any files",
    )
    args = parser.parse_args()

    logger.info("HDC model migration: pickle → bincode")
    logger.info("Input:  %s", args.input)
    logger.info("Output: %s", args.output)

    if args.dry_run:
        logger.info("DRY RUN — no files will be written")

    # Load the pickle model.
    model = load_pickle_model(args.input)
    if model is None:
        if not args.input.exists():
            logger.info(
                "No pickle model found at %s — nothing to migrate. "
                "The Rust server will start with a fresh model.",
                args.input,
            )
            return 0
        return 1

    # Extract parameters.
    params = extract_model_params(model)
    if params is None:
        return 1

    if args.dry_run:
        logger.info("DRY RUN: would write migration JSON with params: %s", params)
        return 0

    # Write migration JSON.
    if not write_migration_json(params, args.output):
        return 1

    print_next_steps(args.input, args.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
