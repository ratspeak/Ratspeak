#!/usr/bin/env python3
"""Regenerate the opportunistic delivery golden from Python LXMF.

Every value comes from the upstream checkouts' own `LXMessage.pack()`: for
messages with no stamp, a ticket stamp and a proof-of-work stamp, the content
lengths on either side of the point where pack() falls back from
opportunistic to direct delivery. The stamp is attached, as in the pack that
sends the message. Each case also records the size pack() gives the same
message without a stamp.

This overwrites crates/ratspeak-runtime/testdata/lxmf-opportunistic-golden.json.
Run with the upstream Reticulum and LXMF checkouts, by default the
`upstream/` directory beside this repository:

    python3 -I tools/generate-lxmf-opportunistic-golden.py [--upstream DIR]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GOLDEN = ROOT / "crates" / "ratspeak-runtime" / "testdata" / "lxmf-opportunistic-golden.json"
RETICULUM = ("1.5.5", "7f2b3b9b524c9386316379af1313b43a5e4f7a5d")
LXMF_RELEASE = ("1.2.0", "c3ff2d6dc2f256daab896dadc044dd5a913ecbb7")
TIMESTAMP = 1700000000.0
TICKET = bytes([0x5A]) * 16
STAMP_COST = 1


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
        epilog="Overwrites crates/ratspeak-runtime/testdata/lxmf-opportunistic-golden.json.",
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

    def delivery(seed: int) -> RNS.Destination:
        identity = RNS.Identity(create_keys=False)
        identity.load_private_key(bytes([seed]) * 64)
        return RNS.Destination(
            identity, RNS.Destination.OUT, RNS.Destination.SINGLE, "lxmf", "delivery"
        )

    destination, source = delivery(0xC3), delivery(0xA1)

    def packed(length: int, stamp: str) -> LXMessage:
        message = LXMessage(
            destination,
            source,
            content="x" * length,
            title="",
            desired_method=LXMessage.OPPORTUNISTIC,
        )
        message.timestamp = TIMESTAMP
        if stamp == "ticket":
            message.outbound_ticket = TICKET
        elif stamp == "proof_of_work":
            message.stamp_cost = STAMP_COST
        message.defer_stamp = False
        message.pack()
        return message

    names = {LXMessage.OPPORTUNISTIC: "opportunistic", LXMessage.DIRECT: "direct"}
    cases = []
    for stamp in ("none", "ticket", "proof_of_work"):
        methods = {length: names[packed(length, stamp).method] for length in range(200, 320)}
        last = max(length for length, method in methods.items() if method == "opportunistic")
        if methods[last + 1] != "direct":
            sys.exit(f"no single fallback point for {stamp}")
        for length in (last, last + 1):
            message = packed(length, stamp)
            cases.append(
                {
                    "stamp": stamp,
                    "content_length": length,
                    "unstamped_packed_size": packed(length, "none").packed_size,
                    "packed_size": message.packed_size,
                    "method": names[message.method],
                }
            )

    golden = {
        "source": "LXMF LXMessage.pack(), defer_stamp False",
        "reticulum": {"version": RETICULUM[0], "commit": RETICULUM[1]},
        "lxmf": {"version": LXMF_RELEASE[0], "commit": LXMF_RELEASE[1]},
        "encrypted_packet_max_content": LXMessage.ENCRYPTED_PACKET_MAX_CONTENT,
        "title": "",
        "stamp_cost": STAMP_COST,
        "cases": cases,
    }
    GOLDEN.parent.mkdir(parents=True, exist_ok=True)
    GOLDEN.write_text(json.dumps(golden, indent=2) + "\n")
    print(f"wrote {GOLDEN.relative_to(ROOT)}: {len(cases)} cases")


if __name__ == "__main__":
    main()
