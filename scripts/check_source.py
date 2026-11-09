#!/usr/bin/env python3
"""Dependency-free structural checks. --rust-syntax additionally needs tree-sitter-rust."""
import argparse
import ast
import json
from pathlib import Path
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def check(rust_syntax=False):
    manifests = list(ROOT.rglob("Cargo.toml"))
    manifests = [p for p in manifests if "target" not in p.relative_to(ROOT).parts]
    # Windows defaults to a legacy code page (for example CP1252).  The
    # repository, configuration examples and workflow are UTF-8 by contract.
    root = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    for member in root["workspace"]["members"]:
        manifest = tomllib.loads((ROOT / member / "Cargo.toml").read_text(encoding="utf-8"))
        assert manifest["package"]["name"] == Path(member).name
        for source in ["lib.rs", "main.rs"]:
            if (ROOT / member / "src" / source).is_file():
                break
        else:
            raise AssertionError(f"No Rust entry point in {member}")
        for spec in manifest.get("dependencies", {}).values():
            if isinstance(spec, dict) and "path" in spec:
                assert (ROOT / member / spec["path"] / "Cargo.toml").is_file()
    examples = list((ROOT / "config").glob("*.example.toml"))
    for path in manifests + examples + [ROOT / "rust-toolchain.toml"]:
        tomllib.loads(path.read_text(encoding="utf-8"))
    for path in examples:
        config = tomllib.loads(path.read_text(encoding="utf-8"))
        for key in ["core", "receiver"]:
            if key in config:
                endpoint = config[key]
                assert "credential_env" in endpoint
                assert "apikey" not in endpoint and "token" not in endpoint
    for path in (ROOT / "scripts").glob("*.py"):
        ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    workflow = (ROOT / ".github/workflows/build.yml").read_text(encoding="utf-8")
    for target in ["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]:
        assert target in workflow
    rust_files = list((ROOT / "crates").rglob("*.rs"))
    errors = []
    if rust_syntax:
        from tree_sitter import Language, Parser
        import tree_sitter_rust
        parser = Parser(Language(tree_sitter_rust.language()))
        for path in rust_files:
            tree = parser.parse(path.read_bytes())
            nodes = [tree.root_node]
            while nodes:
                node = nodes.pop()
                if node.type == "ERROR" or node.is_missing:
                    errors.append(f"{path.relative_to(ROOT)}:{node.start_point}")
                nodes.extend(node.children)
    assert not errors, errors
    print(json.dumps({"cargo_manifests": len(manifests), "config_examples": len(examples),
                      "rust_files": len(rust_files), "rust_syntax_checked": rust_syntax,
                      "status": "passed", "rust_compilation": "not_checked"}, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-syntax", action="store_true")
    check(parser.parse_args().rust_syntax)
