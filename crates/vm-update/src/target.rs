use crate::{Asset, Release};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
    MacOs,
    Android,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X64,
    X86,
    Arm64,
    Arm32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageFormat {
    Zip,
    Msi,
    AppImage,
    Deb,
    Rpm,
    ArchLinux,
    TarGz,
    Dmg,
    Apk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub platform: Platform,
    pub arch: Arch,
    pub preferred_format: Option<PackageFormat>,
}

impl Target {
    pub const fn new(platform: Platform, arch: Arch) -> Self {
        Self {
            platform,
            arch,
            preferred_format: None,
        }
    }

    pub const fn with_format(self, format: PackageFormat) -> Self {
        Self {
            preferred_format: Some(format),
            ..self
        }
    }

    pub fn current() -> Option<Self> {
        let platform = match std::env::consts::OS {
            "windows" => Platform::Windows,
            "linux" => Platform::Linux,
            "macos" => Platform::MacOs,
            "android" => Platform::Android,
            _ => return None,
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X64,
            "x86" => Arch::X86,
            "aarch64" => Arch::Arm64,
            "arm" => Arch::Arm32,
            _ => return None,
        };
        Some(Self::new(platform, arch))
    }

    fn formats(self) -> &'static [PackageFormat] {
        use PackageFormat::*;
        match self.platform {
            Platform::Windows => &[Zip, Msi],
            Platform::Linux => &[AppImage, TarGz, Deb, ArchLinux, Rpm],
            Platform::MacOs => &[Dmg, Zip],
            Platform::Android => &[Apk],
        }
    }

    fn matches(self, format: PackageFormat, universal: bool, name: &str) -> bool {
        use PackageFormat::*;
        let arch_names: &[_] = if universal {
            match (self.platform, self.arch) {
                (Platform::MacOs, Arch::X64 | Arch::Arm64) => &["universal", "universal2"],
                (Platform::Android, Arch::X64 | Arch::Arm64 | Arch::Arm32) => &["universal"],
                _ => return false,
            }
        } else {
            match (self.platform, self.arch) {
                (Platform::Android, Arch::Arm64) => &["arm64-v8a", "arm64", "aarch64"],
                (Platform::Android, Arch::Arm32) => &["armeabi-v7a", "armv7", "arm"],
                (_, Arch::X64) => &["amd64", "x86_64", "x64"],
                (_, Arch::X86) => &["386", "i686", "i386", "x86"],
                (_, Arch::Arm64) => &["arm64", "aarch64"],
                (_, Arch::Arm32) => &["armv7", "armhf"],
            }
        };
        let (prefix, suffix) = match (self.platform, format) {
            (Platform::Windows, Zip) => ("windows-", ".zip"),
            (Platform::Windows, Msi) => ("windows-", ".msi"),
            (Platform::Linux, AppImage) => ("linux-", ".appimage"),
            (Platform::Linux, Deb) => ("linux-", ".deb"),
            (Platform::Linux, Rpm) => ("linux-", ".rpm"),
            (Platform::Linux, ArchLinux) => ("linux-", ".pkg.tar.zst"),
            (Platform::Linux, TarGz) => ("linux-", ".tar.gz"),
            (Platform::MacOs, Dmg) => ("macos-", ".dmg"),
            (Platform::MacOs, Zip) => ("macos-", ".zip"),
            (Platform::Android, Apk) => ("android-", ".apk"),
            _ => return false,
        };
        let Some(name) = name
            .strip_prefix(prefix)
            .or_else(|| (self.platform == Platform::Android && universal).then_some(name))
            .and_then(|name| name.strip_suffix(suffix))
        else {
            return false;
        };
        let arch = if self.platform == Platform::Windows && format == Zip {
            name.strip_suffix("-portable").unwrap_or(name)
        } else {
            name
        };
        arch_names.contains(&arch)
    }
}

impl Release {
    pub fn asset_for(&self, target: Target) -> Option<&Asset> {
        let version = self.version.to_string().to_ascii_lowercase();
        let find = |format, universal| {
            self.assets.iter().find(|asset| {
                let name = asset.name.to_ascii_lowercase();
                let name = name.strip_prefix("velmira-native-").unwrap_or(&name);
                let name = name
                    .strip_prefix(&version)
                    .or_else(|| name.strip_prefix('v')?.strip_prefix(&version))
                    .and_then(|name| name.strip_prefix('-'))
                    .unwrap_or(name);
                target.matches(format, universal, name)
            })
        };
        if let Some(format) = target.preferred_format {
            if let Some(asset) = find(format, false).or_else(|| find(format, true)) {
                return Some(asset);
            }
        }
        [false, true].into_iter().find_map(|universal| {
            target
                .formats()
                .iter()
                .find_map(|&format| find(format, universal))
        })
    }
}
