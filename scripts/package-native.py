"""Assemble a checked portable archive from an existing native release build."""

import argparse
import hashlib
import os
from pathlib import Path
import plistlib
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parents[1]
MAC_TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")
WINDOWS_TARGETS = {
    "x86_64-pc-windows-msvc": ("windows-amd64-portable", 0x8664, 0x20B),
    "i686-pc-windows-msvc": ("windows-386", 0x014C, 0x10B),
    "aarch64-pc-windows-msvc": ("windows-arm64", 0xAA64, 0x20B),
}
LINUX_TARGETS = {
    "x86_64-unknown-linux-gnu": ("linux-amd64", 0x3E, 2, 64),
    "i686-unknown-linux-gnu": ("linux-386", 0x03, 1, 52),
    "aarch64-unknown-linux-gnu": ("linux-arm64", 0xB7, 2, 64),
}
MAC_UNIVERSAL = "universal2-apple-darwin"
TARGETS = (*WINDOWS_TARGETS, *LINUX_TARGETS, MAC_UNIVERSAL)
LICENSE_NAME = re.compile(r"^(licen[sc]e|copying|copyright|notice|ofl|unlicense)", re.I)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def run(*args: str | Path, capture_output: bool = False) -> str:
    completed = subprocess.run(
        args, cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE if capture_output else None
    )
    return completed.stdout or ""


def digest(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def built_binary(build_dir: Path, target: str) -> Path:
    filename = "velmira-native.exe" if "-windows-" in target else "velmira-native"
    return build_dir / target / "release" / filename


def verify_binary(binary: Path, target: str) -> None:
    with binary.open("rb") as source:
        header = source.read(64)
        if target in WINDOWS_TARGETS:
            _, expected_machine, expected_kind = WINDOWS_TARGETS[target]
            require(len(header) == 64 and header.startswith(b"MZ"), f"Not a PE file: {binary}")
            source.seek(struct.unpack_from("<I", header, 60)[0])
            pe = source.read(26)
            require(
                len(pe) == 26
                and pe.startswith(b"PE\0\0")
                and struct.unpack_from("<H", pe, 4)[0] == expected_machine
                and struct.unpack_from("<H", pe, 24)[0] == expected_kind,
                f"PE architecture does not match {target}: {binary}",
            )
        else:
            _, expected_machine, elf_class, header_size = LINUX_TARGETS[target]
            require(
                len(header) >= header_size
                and header.startswith(b"\x7fELF" + bytes((elf_class, 1)))
                and struct.unpack_from("<H", header, 18)[0] == expected_machine,
                f"ELF architecture does not match {target}: {binary}",
            )


def add_dependency_licenses(directory: Path, targets: tuple[str, ...]) -> None:
    packages = set()
    for target in targets:
        tree = run(
            "cargo", "+stable", "tree", "--locked", "--target", target, "--package", "velmira-native",
            "--edges", "normal,build", "--prefix", "none", "--format", "{p}", capture_output=True,
        )
        for line in tree.splitlines():
            # Registry packages have no local path suffix. Repeated nodes carry `(*)`.
            line = line.removesuffix(" (*)")
            match = re.fullmatch(r"([\w-]+) v([^\s]+)", line)
            if match:
                packages.add(match.groups())
    require(bool(packages), "No registry packages found in Cargo dependency tree")
    registry = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")) / "registry" / "src"
    notices = [
        "# Third-party Rust components",
        "",
        "License files supplied by the listed packages are included under `licenses/`.",
        "",
    ]
    for name, version in sorted(packages):
        source = next(registry.glob(f"*/{name}-{version}"), None)
        if source is None:
            raise FileNotFoundError(f"Cargo source unavailable for {name} {version}")
        metadata = tomllib.loads((source / "Cargo.toml").read_text(encoding="utf-8"))["package"]
        license_file = metadata.get("license-file")
        license_kind = metadata.get("license", license_file or "See upstream source")
        notices.append(f"- {name} {version} — {license_kind} — https://crates.io/crates/{name}/{version}")
        files = {path for path in source.iterdir() if path.is_file() and LICENSE_NAME.match(path.name)}
        if license_file:
            files.add(source / license_file)
        if files:
            destination = directory / "licenses" / f"{name}-{version}"
            destination.mkdir(parents=True, exist_ok=True)
            for path in sorted(files):
                shutil.copy2(path, destination / path.name)
    (directory / "THIRD-PARTY.md").write_text("\n".join(notices) + "\n", encoding="utf-8")


def make_bundle(directory: Path, build_dir: Path, version: str) -> Path:
    bundle = directory / "Velmira Native.app"
    executable = bundle / "Contents" / "MacOS" / "velmira-native"
    executable.parent.mkdir(parents=True)
    inputs = [built_binary(build_dir, target) for target in MAC_TARGETS]
    run("lipo", "-create", *inputs, "-output", executable)
    arches = run("lipo", "-archs", executable, capture_output=True).split()
    require(set(arches) == {"arm64", "x86_64"}, "macOS bundle lacks a required architecture")
    executable.chmod(0o755)
    metadata = {
        "CFBundleIdentifier": "dev.velmira.native",
        "CFBundleName": "Velmira Native",
        "CFBundleExecutable": executable.name,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": re.split(r"[-+]", version, maxsplit=1)[0],
        "CFBundleVersion": re.split(r"[-+]", version, maxsplit=1)[0],
        "LSMinimumSystemVersion": os.environ.get("MACOSX_DEPLOYMENT_TARGET", "11.0"),
        "NSHighResolutionCapable": True,
    }
    (bundle / "Contents" / "Info.plist").write_bytes(plistlib.dumps(metadata))
    run("codesign", "--force", "--sign", "-", bundle)
    run("codesign", "--verify", "--strict", bundle)
    return executable


def write_archive(directory: Path, archive: Path, executable: Path, target: str) -> None:
    member_name = executable.relative_to(directory.parent).as_posix()
    if target == MAC_UNIVERSAL:
        run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", directory, archive)
    elif target in WINDOWS_TARGETS:
        with zipfile.ZipFile(
            archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9, strict_timestamps=False
        ) as output:
            for path in sorted(directory.rglob("*")):
                if path.is_file():
                    output.write(path, path.relative_to(directory.parent))
    else:
        with tarfile.open(archive, "w:gz") as output:
            output.add(directory, arcname=directory.name)

    if target in LINUX_TARGETS:
        with tarfile.open(archive, "r:gz") as output:
            member = output.getmember(member_name)
            require(member.isfile() and bool(member.mode & 0o111), f"Executable mode lost: {member_name}")
            with output.extractfile(member) as source:
                archived_digest = hashlib.file_digest(source, "sha256").hexdigest()
    else:
        with zipfile.ZipFile(archive) as output:
            require(output.testzip() is None, f"Corrupt ZIP archive: {archive}")
            with output.open(member_name) as source:
                archived_digest = hashlib.file_digest(source, "sha256").hexdigest()
    require(archived_digest == digest(executable), f"Archived executable differs: {member_name}")


def package(target: str, build_dir: Path, output_dir: Path) -> None:
    package_config = tomllib.loads((ROOT / "apps" / "desktop" / "Cargo.toml").read_text(encoding="utf-8"))
    version = package_config["package"]["version"]
    name = f"Velmira-Native-{version}"
    if target == MAC_UNIVERSAL:
        suffix = "macos-universal2.zip"
        targets = MAC_TARGETS
        instructions = (
            "打开 Velmira Native.app。此预览包使用临时签名，尚未通过 Apple Developer ID 签名和公证。"
        )
    elif target in WINDOWS_TARGETS:
        suffix = WINDOWS_TARGETS[target][0] + ".zip"
        targets = (target,)
        instructions = "解压后运行 velmira-native.exe。"
    else:
        suffix = LINUX_TARGETS[target][0] + ".tar.gz"
        targets = (target,)
        instructions = (
            "解压后运行 ./velmira-native。需要 X11 或 Wayland、OpenGL 驱动。"
            "文件选择对话框需要 XDG Desktop Portal，也可以拖入文件。"
        )

    output_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".package-native-", dir=output_dir) as temporary:
        staging = Path(temporary)
        directory = staging / name
        directory.mkdir()
        if target == MAC_UNIVERSAL:
            executable = make_bundle(directory, build_dir, version)
        else:
            binary = built_binary(build_dir, target)
            verify_binary(binary, target)
            executable = directory / binary.name
            shutil.copy2(binary, executable)
            if target in LINUX_TARGETS:
                executable.chmod(0o755)

        shutil.copy2(ROOT / "LICENSE", directory / "LICENSE")
        add_dependency_licenses(directory, targets)
        font_license = directory / "licenses" / "VelmiraCJK-OFL.txt"
        font_license.parent.mkdir(exist_ok=True)
        shutil.copy2(ROOT / "apps" / "desktop" / "assets" / "OFL.txt", font_license)
        (directory / "START-HERE.txt").write_text(
            f"Velmira Native {version}\n\n{instructions}\n\n"
            "打开后点击「导入书籍」，或把 TXT、EPUB、CBZ 拖入窗口。\n"
            "https://github.com/sakucyber/Velmira\n",
            encoding="utf-8-sig",
        )
        archive = staging / f"{name}-{suffix}"
        write_archive(directory, archive, executable, target)
        checksum = staging / f"{archive.name}.sha256"
        checksum.write_text(f"{digest(archive)}  {archive.name}\n", encoding="ascii")
        for path in (archive, checksum):
            result = path.replace(output_dir / path.name)
            print(result)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--build-dir", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")))
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    package(args.target, args.build_dir.resolve(), args.output_dir.resolve())


if __name__ == "__main__":
    main()
