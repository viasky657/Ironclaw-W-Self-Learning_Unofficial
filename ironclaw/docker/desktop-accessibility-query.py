#!/usr/bin/env python3
"""
AT-SPI2 accessibility tree query tool.

Queries the AT-SPI2 accessibility bus and outputs a structured JSON
representation of the current UI state. This is the safe interface for
the AI to observe desktop app state — it never gets raw X11 socket access.

Usage:
    desktop-accessibility-query.py [--app APP_NAME] [--max-depth N]

Output (stdout): JSON object with the accessibility tree.
Errors (stderr): Human-readable error messages.

Security notes:
    - This script runs inside the desktop sandbox container only.
    - It queries AT-SPI2 (structured UI state), not raw X11 events.
    - Output is sanitised: no raw pixel data, no clipboard content.
    - Sensitive field values (password fields) are redacted.
"""

import json
import sys
import argparse

try:
    import pyatspi
except ImportError:
    print(
        json.dumps({
            "error": "pyatspi not available",
            "detail": "Install python3-pyatspi inside the desktop sandbox container.",
        }),
        file=sys.stdout,
    )
    sys.exit(1)


# Role names that indicate sensitive input fields — values are redacted.
SENSITIVE_ROLES = {
    pyatspi.ROLE_PASSWORD_TEXT,
}

# Maximum number of nodes to include in the tree (prevents runaway output).
MAX_NODES = 500


def redact_if_sensitive(role, text: str) -> str:
    """Redact text content for sensitive roles (e.g. password fields)."""
    if role in SENSITIVE_ROLES:
        return "[REDACTED]"
    return text


def accessible_to_dict(acc, depth: int, max_depth: int, node_count: list) -> dict:
    """
    Recursively convert an AT-SPI2 Accessible object to a JSON-serialisable dict.

    Args:
        acc: pyatspi.Accessible object.
        depth: Current recursion depth.
        max_depth: Maximum recursion depth.
        node_count: Single-element list used as a mutable counter.

    Returns:
        dict with role, name, description, states, value, children.
    """
    if node_count[0] >= MAX_NODES:
        return {"truncated": True, "reason": f"max_nodes={MAX_NODES} reached"}

    node_count[0] += 1

    try:
        role = acc.getRole()
        role_name = acc.getRoleName()
        name = acc.name or ""
        description = acc.description or ""

        # State set
        state_set = acc.getState()
        states = [
            pyatspi.stateToString(s)
            for s in pyatspi.STATE_VALUE_TO_NAME.keys()
            if state_set.contains(s)
        ]

        # Text content (if available)
        text_value = None
        try:
            text_iface = acc.queryText()
            raw = text_iface.getText(0, -1)
            text_value = redact_if_sensitive(role, raw)
        except (NotImplementedError, AttributeError):
            pass

        # Value (e.g. sliders, progress bars)
        numeric_value = None
        try:
            val_iface = acc.queryValue()
            numeric_value = val_iface.currentValue
        except (NotImplementedError, AttributeError):
            pass

        # Bounding box (screen coordinates inside the virtual display)
        bounds = None
        try:
            component = acc.queryComponent()
            ext = component.getExtents(pyatspi.DESKTOP_COORDS)
            bounds = {
                "x": ext.x,
                "y": ext.y,
                "width": ext.width,
                "height": ext.height,
            }
        except (NotImplementedError, AttributeError):
            pass

        node: dict = {
            "role": role_name,
            "name": name,
        }
        if description:
            node["description"] = description
        if states:
            node["states"] = states
        if text_value is not None:
            node["text"] = text_value
        if numeric_value is not None:
            node["value"] = numeric_value
        if bounds:
            node["bounds"] = bounds

        # Recurse into children
        if depth < max_depth and acc.childCount > 0:
            children = []
            for i in range(acc.childCount):
                try:
                    child = acc.getChildAtIndex(i)
                    if child is not None:
                        child_dict = accessible_to_dict(
                            child, depth + 1, max_depth, node_count
                        )
                        children.append(child_dict)
                except Exception:  # noqa: BLE001
                    children.append({"error": "could not access child"})
            if children:
                node["children"] = children

        return node

    except Exception as exc:  # noqa: BLE001
        return {"error": str(exc)}


def get_desktop_tree(app_name: str | None, max_depth: int) -> dict:
    """
    Query the AT-SPI2 desktop and return the accessibility tree as a dict.

    Args:
        app_name: If set, only include the named application.
        max_depth: Maximum tree depth to traverse.

    Returns:
        dict with "applications" list and optional "error".
    """
    try:
        desktop = pyatspi.Registry.getDesktop(0)
    except Exception as exc:  # noqa: BLE001
        return {"error": f"Could not connect to AT-SPI2 registry: {exc}"}

    applications = []
    node_count = [0]

    for app in desktop:
        if app is None:
            continue
        app_name_actual = app.name or ""
        if app_name and app_name.lower() not in app_name_actual.lower():
            continue

        app_dict = accessible_to_dict(app, depth=0, max_depth=max_depth, node_count=node_count)
        app_dict["application"] = app_name_actual
        applications.append(app_dict)

        if node_count[0] >= MAX_NODES:
            break

    return {
        "applications": applications,
        "node_count": node_count[0],
        "truncated": node_count[0] >= MAX_NODES,
    }


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Query AT-SPI2 accessibility tree and output JSON."
    )
    parser.add_argument(
        "--app",
        default=None,
        help="Filter by application name (case-insensitive substring match).",
    )
    parser.add_argument(
        "--max-depth",
        type=int,
        default=10,
        help="Maximum tree depth to traverse (default: 10).",
    )
    args = parser.parse_args()

    result = get_desktop_tree(app_name=args.app, max_depth=args.max_depth)
    print(json.dumps(result, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
