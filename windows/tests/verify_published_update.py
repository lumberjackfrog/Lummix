"""Verify the published manifest, GitHub asset metadata and downloaded installer together."""
import argparse
import hashlib
from pathlib import Path
import update_feed as feed


if __name__=="__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("tag")
    parser.add_argument("setup",type=Path)
    args=parser.parse_args()
    release=feed.published(args.tag)
    with args.setup.open("rb") as exe:
        assert args.setup.stat().st_size==release.size
        assert hashlib.file_digest(exe,"sha256").hexdigest()==release.digest
    print("Verified publisher signature and installer",args.tag,release.digest)
