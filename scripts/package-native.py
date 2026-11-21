"""Assemble a checked portable archive from an existing native release build."""

import argparse
import gzip
import hashlib
import io
import lzma
import os
from pathlib import Path
import plistlib
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import time
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
LINUX_DISTRO_ARCHES = {
    "x86_64-unknown-linux-gnu": ("amd64", "x86_64"),
    "i686-unknown-linux-gnu": ("i386", "i686"),
}
APP_NAME = "velmira-native"
APP_URL = "https://github.com/sakucyber/Velmira"
APP_DESCRIPTION = "Local-first desktop reader for TXT, EPUB and CBZ"


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


def installed_files(directory: Path, executable: Path) -> dict[str, tuple[bytes, int]]:
    files = {f"usr/bin/{APP_NAME}": (executable.read_bytes(), 0o755)}
    entry = (
        "[Desktop Entry]\nType=Application\nName=Velmira Native\n"
        f"Comment={APP_DESCRIPTION}\nExec={APP_NAME}\n"
        "Terminal=false\nCategories=Office;Literature;\nStartupNotify=true\n"
    )
    files[f"usr/share/applications/{APP_NAME}.desktop"] = (entry.encode(), 0o644)
    for source in sorted((directory / "licenses").rglob("*")):
        if source.is_file():
            relative = source.relative_to(directory / "licenses").as_posix()
            files[f"usr/share/licenses/{APP_NAME}/{relative}"] = (source.read_bytes(), 0o644)
    for name in ("LICENSE", "THIRD-PARTY.md"):
        files[f"usr/share/licenses/{APP_NAME}/{name}"] = ((directory / name).read_bytes(), 0o644)
    return files


def parent_dirs(files: dict[str, tuple[bytes, int]]) -> list[str]:
    return sorted({
        name[:index]
        for name in files
        for index, character in enumerate(name)
        if character == "/"
    })


def tar_bytes(files: dict[str, tuple[bytes, int]], epoch: int) -> bytes:
    storage = io.BytesIO()
    # GNU extensions preserve long license filenames without PAX entries that older dpkg rejects.
    with tarfile.open(fileobj=storage, mode="w", format=tarfile.GNU_FORMAT) as archive:
        for name in parent_dirs(files):
            item = tarfile.TarInfo(name)
            item.type, item.mode = tarfile.DIRTYPE, 0o755
            item.uname, item.gname, item.mtime = "root", "root", epoch
            archive.addfile(item)
        for name, (content, mode) in sorted(files.items()):
            item = tarfile.TarInfo(name)
            item.size, item.mode = len(content), mode
            item.uname, item.gname, item.mtime = "root", "root", epoch
            archive.addfile(item, io.BytesIO(content))
    return storage.getvalue()


def encode_debian(
    output: Path, files: dict[str, tuple[bytes, int]], version: str, arch: str, epoch: int
) -> None:
    files[f"usr/share/doc/{APP_NAME}/copyright"] = files[f"usr/share/licenses/{APP_NAME}/LICENSE"]
    installed_size = sum((len(content) + 1023) // 1024 for content, _ in files.values())
    control = (
        f"Package: {APP_NAME}\nVersion: {version}-1\nArchitecture: {arch}\n"
        "Maintainer: Velmira contributors <160121170+sakucyber@users.noreply.github.com>\n"
        "Section: text\nPriority: optional\n"
        "Depends: libc6 (>= 2.35), libgcc-s1, libgl1, libegl1, libwayland-client0, "
        "libwayland-cursor0, libwayland-egl1, libxkbcommon0, libxkbcommon-x11-0, "
        "libx11-6, libxcursor1, libxi6, libxrandr2\n"
        "Recommends: xdg-desktop-portal, xdg-utils\n"
        f"Homepage: {APP_URL}\nInstalled-Size: {installed_size}\nDescription: {APP_DESCRIPTION}\n"
        " Reads ebooks and comics in a native desktop window and stores the library locally.\n"
    )
    hashes = "".join(
        f"{hashlib.md5(content, usedforsecurity=False).hexdigest()}  {name}\n"
        for name, (content, _) in sorted(files.items())
    ).encode()
    metadata = {"control": (control.encode(), 0o644), "md5sums": (hashes, 0o644)}
    with output.open("wb") as archive:
        archive.write(b"!<arch>\n")
        for name, content in (
            ("debian-binary", b"2.0\n"),
            ("control.tar.xz", lzma.compress(tar_bytes(metadata, epoch))),
            ("data.tar.xz", lzma.compress(tar_bytes(files, epoch))),
        ):
            header = f"{name + '/':<16}{epoch:<12}{0:<6}{0:<6}{'100644':<8}{len(content):<10}`\n"
            require(len(header) == 60, "Invalid Debian archive header")
            archive.write(header.encode("ascii"))
            archive.write(content)
            if len(content) % 2:
                archive.write(b"\n")


def mtree_escape(path: str) -> str:
    return "".join(
        chr(byte) if byte < 128 and (chr(byte).isalnum() or byte in b"./_+-")
        else f"\\{byte:03o}"
        for byte in path.encode()
    )


def encode_arch(
    output: Path, files: dict[str, tuple[bytes, int]], version: str, arch: str, epoch: int
) -> None:
    dependencies = (
        "glibc>=2.35", "gcc-libs", "libglvnd", "wayland", "libxkbcommon", "libxkbcommon-x11", "libx11",
        "libxcursor", "libxi", "libxrandr",
    )
    installed_size = sum(len(content) for content, _ in files.values())
    metadata = (
        f"pkgname = {APP_NAME}\npkgbase = {APP_NAME}\npkgver = {version}-1\n"
        f"pkgdesc = {APP_DESCRIPTION}\nurl = {APP_URL}\nbuilddate = {epoch}\n"
        f"packager = Velmira contributors\nsize = {installed_size}\narch = {arch}\nlicense = MIT\n"
        + "".join(f"depend = {package}\n" for package in dependencies)
        + "optdepend = xdg-desktop-portal: file selection dialog\n"
        + "optdepend = xdg-utils: opening web links\n"
    )
    files[".PKGINFO"] = (metadata.encode(), 0o644)
    mtree = ["#mtree", "/set type=file uid=0 gid=0 mode=644"]
    mtree.extend(f"{mtree_escape(name)} type=dir mode=755 time={epoch}.0" for name in [
        ".", *(f"./{name}" for name in parent_dirs(files)),
    ])
    mtree.extend(
        f"./{mtree_escape(name)} mode={mode:o} size={len(content)} time={epoch}.0 "
        f"sha256digest={hashlib.sha256(content).hexdigest()}"
        for name, (content, mode) in sorted(files.items())
    )
    files[".MTREE"] = (gzip.compress(("\n".join(mtree) + "\n").encode(), mtime=epoch), 0o644)
    temporary_tar = output.with_suffix("")
    temporary_tar.write_bytes(tar_bytes(files, epoch))
    run("zstd", "-q", "-f", "-10", temporary_tar, "-o", output)
    run("zstd", "-q", "--test", output)


def linux_distribution_packages(
    archive: Path, directory: Path, executable: Path, target: str, version: str
) -> tuple[Path, Path]:
    deb_arch, pacman_arch = LINUX_DISTRO_ARCHES[target]
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", int(time.time())))
    require(0 <= epoch < 2**32, "SOURCE_DATE_EPOCH is outside the supported range")
    core, _, prerelease = version.split("+", 1)[0].partition("-")
    deb_version = f"{core}~{prerelease}" if prerelease else core
    arch_version = f"{core}{prerelease.replace('-', '.')}" if prerelease else core
    files = installed_files(directory, executable)
    deb = archive.with_name(archive.name.removesuffix(".tar.gz") + ".deb")
    arch = archive.with_name(archive.name.removesuffix(".tar.gz") + ".pkg.tar.zst")
    encode_debian(deb, files.copy(), deb_version, deb_arch, epoch)
    encode_arch(arch, files, arch_version, pacman_arch, epoch)
    return deb, arch


def package(target: str, build_dir: Path, output_dir: Path, distributions: bool) -> None:
    package_config = tomllib.loads((ROOT / "apps" / "desktop" / "Cargo.toml").read_text(encoding="utf-8"))
    version = package_config["package"]["version"]
    require(not distributions or target in LINUX_DISTRO_ARCHES, "Debian and Arch packages require Linux x64 or i686")
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
            f"{APP_URL}\n",
            encoding="utf-8-sig",
        )
        archive = staging / f"{name}-{suffix}"
        write_archive(directory, archive, executable, target)
        packages = [archive]
        if distributions:
            packages.extend(linux_distribution_packages(archive, directory, executable, target, version))
        for package_path in packages:
            checksum = staging / f"{package_path.name}.sha256"
            checksum.write_text(f"{digest(package_path)}  {package_path.name}\n", encoding="ascii")
            for path in (package_path, checksum):
                result = path.replace(output_dir / path.name)
                print(result)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--build-dir", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")))
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    parser.add_argument("--linux-distributions", action="store_true", help="Also create Linux x64/i686 Debian and Arch packages")
    args = parser.parse_args()
    package(args.target, args.build_dir.resolve(), args.output_dir.resolve(), args.linux_distributions)


if __name__ == "__main__":
    main()
