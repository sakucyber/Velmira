"""Copy the licenses of the Python/Go dependencies present in a desktop build."""
from __future__ import annotations

import argparse
import ast
import importlib.metadata as metadata
import json
from pathlib import Path
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundle", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    args = parser.parse_args()
    output = args.bundle / "licenses"
    output.mkdir(parents=True, exist_ok=True)
    inventory = []

    def collect(name, version, files):
        target = output / name.replace("/", "_")
        target.mkdir(exist_ok=True)
        copied = []
        for source in files:
            if source.is_file():
                dest = target / (str(len(copied)) + "_" + source.name)
                shutil.copyfile(source, dest)
                copied.append(dest.relative_to(output).as_posix())
        if not copied:
            raise RuntimeError("No license found for " + name)
        inventory.append({"name": name, "version": version, "licenses": copied})

    collect("Python", sys.version.split()[0], [Path(sys.base_prefix) / "LICENSE.txt"])
    toc = ast.literal_eval((args.root / ".tmp/pyi-work/Velmira-backend/PYZ-00.toc").read_text(encoding="utf-8"))
    modules = {entry[0].split(".")[0] for entry in toc[1]}
    owners = metadata.packages_distributions()
    distributions = {name for module in modules for name in owners.get(module, [])} | {"pyinstaller"}
    for name in sorted(distributions, key=str.lower):
        dist = metadata.distribution(name)
        files = [Path(dist.locate_file(file)) for file in dist.files or []
                 if any(token in file.name.lower() for token in ("license", "copying", "notice"))]
        collect(name, dist.version, files)

    go = shutil.which("go") or str(args.root / ".toolchains/go/bin/go.exe")

    def run(*command):
        return subprocess.check_output([go, *command], text=True, encoding="utf-8").strip()

    goenv = json.loads(run("env", "-json", "GOROOT", "GOMODCACHE"))
    build = run("version", "-m", str(args.bundle / "Velmira.exe"))
    collect("Go", build.splitlines()[0].rsplit(" ", 1)[-1], [Path(goenv["GOROOT"]) / "LICENSE"])
    for line in build.splitlines():
        fields = line.split()
        if not fields or fields[0] != "dep":
            continue
        name, version = fields[1:3]
        # Go escapes uppercase letters in module cache paths as !lowercase.
        escaped = "".join("!" + char.lower() if char.isupper() else char for char in name)
        directory = Path(goenv["GOMODCACHE"]) / (escaped + "@" + version)
        files = [file for file in directory.iterdir()
                 if file.is_file() and file.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "PATENTS"))]
        collect(name, version, files)
    (output / "DEPENDENCIES.json").write_text(json.dumps(inventory, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print("Bundled dependency licenses: " + str(len(inventory)))


if __name__ == "__main__":
    main()
