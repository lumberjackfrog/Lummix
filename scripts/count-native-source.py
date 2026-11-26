"""Inventory first-party runtime source represented in a particular CI Windows installer.

Use Python 3.12 on Windows, 7-Zip, PyInstaller, tree-sitter and tree-sitter-rust.
Pass the checkout SHA and the installer SHA-256 printed by its build.
"""
import argparse
from collections import Counter
from hashlib import sha256
import json
import msilib
from pathlib import Path, PurePosixPath
import shutil
import subprocess
from tempfile import TemporaryDirectory

from PyInstaller.archive.readers import CArchiveReader
from tree_sitter import Language, Parser
import tree_sitter_rust

ROOT = Path(__file__).resolve().parents[1]
RUST = Parser(Language(tree_sitter_rust.language()))
NATIVE = {
    "windows/dsp/src": "_internal/lummix_dsp.dll",
    "windows/media_native/src": "_internal/lummix_media.dll",
    "windows/host-rs/src": "_internal/lummix_host.dll",
    "windows/controls-rs/src": "studio/controls.wasm",
}
VENDOR = "studio/visualizer-simplex-noise-4.0.3.js"
SCRIPT_TYPES = {".js", ".css", ".html", ".ps1"}


def git(*args):
    return subprocess.check_output(["git", "-C", ROOT, *args])


def file_id(name):
    return "f" + sha256(name.encode()).hexdigest()[:28]


def file_table(msi):
    db = msilib.OpenDatabase(str(msi), msilib.MSIDBOPEN_READONLY)
    query = db.OpenView("SELECT `File`,`FileName` FROM `File`")
    query.Execute(None)
    table = {}
    while row := query.Fetch():
        table[row.GetString(1)] = row.GetString(2).split("|")[-1]
    query.Close()
    db.Close()
    return table


def rust_lines(source):
    tree = RUST.parse(source)
    if tree.root_node.has_error:
        raise ValueError("Could not parse Rust source")
    excluded = set()

    def visit(parent):
        attributes = []
        for node in parent.named_children:
            if node.type == "attribute_item":
                attributes.append(node)
                continue
            if node.type in ("line_comment", "block_comment"):
                continue
            dev_only = any(a.text.replace(b" ", b"") in
                           (b"#[cfg(test)]", b"#[test]", b'#[cfg(feature="replay")]') for a in attributes)
            if dev_only:
                excluded.update(range(attributes[0].start_point.row, node.end_point.row + 1))
            elif node.named_child_count:
                visit(node)
            attributes.clear()

    visit(tree.root_node)
    return sum(bool(line.strip()) for i, line in enumerate(source.splitlines()) if i not in excluded)


def inspect(args, temp):
    archive = shutil.which("7z") or r"C:\Program Files\7-Zip\7z.exe"

    def extract(file, names):
        subprocess.run([archive, "e", "-aoa", "-bso0", f"-o{temp}", "--", str(file), *names], check=True)

    checksum = sha256(args.installer.read_bytes()).hexdigest()
    if checksum.lower() != args.sha256.lower():
        raise ValueError("Installer SHA-256 differs from the build evidence")
    ref = git("rev-parse", "--verify", args.commit+"^{commit}").decode().strip()
    files = git("ls-tree", "-r", "--name-only", ref).decode().splitlines()
    # NSIS stores the MSI beneath its plugin directory.
    subprocess.run([archive, "e", "-aoa", "-bso0", f"-o{temp}", "-ir!Lummix.msi",
                    "--", str(args.installer.resolve())], check=True)
    msi = temp / "Lummix.msi"
    table = file_table(msi)

    def packaged(name):
        identity = file_id(name)
        if table.get(identity) != PurePosixPath(name).name:
            raise ValueError(f"Missing installer payload {name}")
        return identity

    native = {p: packaged(p) for p in [*NATIVE.values(), "Lummix Host.exe", "_internal/controls.wasm"]}
    exe = packaged("Lummix.exe")
    assets = {}
    for name in files:
        path = PurePosixPath(name)
        if path.suffix not in SCRIPT_TYPES or name == VENDOR:
            continue
        if path.parent == PurePosixPath("studio"):
            assets[name] = packaged(name)
        elif path.parent == PurePosixPath("windows"):
            assets[name] = packaged("_internal/" + path.name)
    extract(msi, [exe, *native.values(), *assets.values()])
    payloads = {}
    for name, identity in native.items():
        payload = (temp / identity).read_bytes()
        if not payload.startswith(b"\0asm" if name.endswith(".wasm") else b"MZ"):
            raise ValueError(f"Wrong binary format for {name}")
        payloads[name] = sha256(payload).hexdigest()
    if payloads["studio/controls.wasm"] != payloads["_internal/controls.wasm"]:
        raise ValueError("Desktop and wrist use different control binaries")

    frozen = CArchiveReader(str(temp / exe))
    pyz, = (name for name, entry in frozen.toc.items() if entry[-1] == "z")
    modules = set(frozen.open_embedded_archive(pyz).toc) | set(frozen.toc)
    python = [p for p in map(PurePosixPath, files) if p.parent.as_posix() in ("windows","windows/runtime")
              and p.suffix == ".py" and p.stem in modules]
    if not {"native_audio", "cable_mixer", "runtime_main"} <= {p.stem for p in python}:
        raise ValueError("Expected mixer modules missing from the frozen Python archive")
    rust = [p for p in files if p.endswith(".rs") and any(p.startswith(root+"/") for root in NATIVE)
            and p != "windows/dsp/src/replay.rs"]
    inventory = {}
    by_type = Counter()
    for name in sorted([*assets, *(x.as_posix() for x in python), *rust]):
        source = git("show", ref + ":" + name).replace(b"\r\n", b"\n")
        if name in assets and (temp / assets[name]).read_bytes().replace(b"\r\n", b"\n") != source:
            raise ValueError(f"Packaged asset differs from checkout {name}")
        suffix = PurePosixPath(name).suffix
        lines = rust_lines(source) if suffix == ".rs" else sum(bool(x.strip()) for x in source.splitlines())
        inventory[name] = lines
        by_type[suffix] += lines
    total = sum(by_type.values())
    return {"commit": ref, "installer_sha256": checksum, "installer_bytes": args.installer.stat().st_size,
            "bundles_numpy": "numpy" in modules, "bundles_pillow": "PIL" in modules, "native_payload_sha256": payloads,
            "method": "Nonblank first-party runtime lines including comments. Checks installer payloads and frozen Python module membership. Excludes vendored assets, tests, Rust test-only items and replay injection, and build/packaging files. WASM is counted once from Rust source.",
            "counts": dict(sorted(by_type.items())), "total": total, "rust_percent": 100 * by_type[".rs"] / total,
            "files": inventory}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--installer", required=True, type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    with TemporaryDirectory() as area:
        report = inspect(args, Path(area))
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({k: report[k] for k in ("commit","installer_sha256","counts","total","rust_percent")}))
