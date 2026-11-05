#!/usr/bin/env python3
"""Regenerate the FIELD_THREAD golden from Python LXMF.

The packed bytes come from the upstream checkouts' own `LXMessage.pack()` for a
message in a thread (`fields[FIELD_THREAD] = <32-byte thread ID>`, the full hash
length the field's upstream comment names). This overwrites
crates/ratspeak-runtime/testdata/lxmf-thread-golden.json.
Run with the upstream Reticulum and LXMF checkouts, by
default the `upstream/` directory beside this repository:

    python3 -I tools/generate-lxmf-thread-golden.py [--upstream DIR]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "crates" / "ratspeak-runtime" / "testdata" / "lxmf-thread-golden.json"
RETICULUM = ("1.5.5", "7f2b3b9b524c9386316379af1313b43a5e4f7a5d")
LXMF_RELEASE = ("1.2.0", "c3ff2d6dc2f256daab896dadc044dd5a913ecbb7")
TIMESTAMP = 1700000000.0
CONTENT = "Day 2 avoids the east gully"
THREAD = bytes(range(0x40, 0x60))


def head(checkout: Path) -> str:
    return subprocess.run(
        ["git", "-C", str(checkout), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog="Overwrites crates/ratspeak-runtime/testdata/lxmf-thread-golden.json.",
    )
    parser.add_argument("--upstream", type=Path, default=ROOT.parent / "upstream",
                        help="parent of the pinned Reticulum and LXMF checkouts; defaults to ../upstream relative to this repository")
    args = parser.parse_args()
    reticulum = args.upstream.resolve() / "Reticulum"
    lxmf = args.upstream.resolve() / "LXMF"
    for checkout, (_, commit) in ((reticulum, RETICULUM), (lxmf, LXMF_RELEASE)):
        if head(checkout) != commit:
            sys.exit(f"{checkout} is not at {commit}")
    sys.dont_write_bytecode = True
    sys.path[:0] = [str(reticulum), str(lxmf)]

    import RNS
    import LXMF
    from LXMF.LXMessage import LXMessage

    if RNS.__version__ != RETICULUM[0] or LXMF.__version__ != LXMF_RELEASE[0]:
        sys.exit(f"imported RNS {RNS.__version__} and LXMF {LXMF.__version__}")
    RNS.loglevel = RNS.LOG_CRITICAL

    def delivery(seed: int) -> tuple[RNS.Identity, RNS.Destination]:
        identity = RNS.Identity(create_keys=False)
        identity.load_private_key(bytes([seed]) * 64)
        destination = RNS.Destination(
            identity, RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery"
        )
        return identity, destination

    _, destination = delivery(0xC3)
    source_identity, source = delivery(0xA1)
    message = LXMessage(
        destination,
        source,
        content=CONTENT,
        title="",
        fields={LXMF.FIELD_THREAD: THREAD},
        desired_method=LXMessage.DIRECT,
    )
    message.timestamp = TIMESTAMP
    message.defer_stamp = False
    message.pack()

    golden = {
        "source": "LXMF LXMessage.pack(), fields {FIELD_THREAD: 32-byte thread ID}",
        "reticulum": {"version": RETICULUM[0], "commit": RETICULUM[1]},
        "lxmf": {"version": LXMF_RELEASE[0], "commit": LXMF_RELEASE[1]},
        "timestamp": TIMESTAMP,
        "content": CONTENT,
        "thread": THREAD.hex(),
        "source_public_key": source_identity.get_public_key().hex(),
        "packed": message.packed.hex(),
    }
    GOLDEN.parent.mkdir(parents=True, exist_ok=True)
    GOLDEN.write_text(json.dumps(golden, indent=2) + "\n")
    print(f"wrote {GOLDEN.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
