#!/usr/bin/env python3
"""Append one portable activity event; never modify the dispatcher's ledger."""

import argparse
import datetime
import fcntl
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path", type=Path)
    parser.add_argument("phase", choices=(
        "investigate", "implement", "check", "commit", "land", "done", "review", "gate", "verify",
    ))
    parser.add_argument("text", help="Short activity summary without secrets or raw output")
    args = parser.parse_args()
    event = {
        "type": "dispatch",
        "timestamp": datetime.datetime.now(datetime.UTC).isoformat(timespec="milliseconds"),
        "phase": args.phase,
        "text": args.text,
    }
    args.path.parent.mkdir(parents=True, exist_ok=True)
    with args.path.open("a", encoding="utf-8") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        stream.write(json.dumps(event, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
