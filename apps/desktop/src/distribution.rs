// How this build is distributed (T-STORE-08). Shared by `build.rs` (through `include!`, so
// plain `//` comments only) and the app (`crate::distribution`).
//
// The packaging step sets `DEMIOS_DISTRIBUTION` when it builds:
// - unset or `direct`: a download from GitHub Releases (DMG, MSI/NSIS, AppImage);
// - `msix`: the Microsoft Store package;
// - `flatpak`: the Flathub package.
// Store and Flathub builds are updated by the store, so they must not contain the
// self-updater: `build.rs` fails the build when the `updater` feature is on for them.

/// Where a build is distributed, from `DEMIOS_DISTRIBUTION` at build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distribution {
    /// Direct download from GitHub Releases (the default).
    Direct,
    /// Microsoft Store (MSIX): the Store updates the app.
    MicrosoftStore,
    /// Flathub: Flatpak updates the app.
    Flathub,
}

impl Distribution {
    /// Parses `DEMIOS_DISTRIBUTION`; unset or empty means a direct download. An unknown value
    /// is an error, so a typo can't turn a store build into a self-updating one.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.map(str::trim) {
            None | Some("") | Some("direct") => Ok(Self::Direct),
            Some("msix") => Ok(Self::MicrosoftStore),
            Some("flatpak") => Ok(Self::Flathub),
            Some(other) => Err(format!(
                "DEMIOS_DISTRIBUTION={other:?} is not one of direct, msix, flatpak"
            )),
        }
    }

    /// Whether a build for this distribution may contain the self-updater.
    pub fn may_self_update(self) -> bool {
        self == Self::Direct
    }
}

/// Checks a build's settings: `Err` (the build fails) when the self-updater is on for a
/// distribution whose store updates the app, or the distribution is unknown.
pub fn check_build(updater_feature: bool, value: Option<&str>) -> Result<Distribution, String> {
    let d = Distribution::parse(value)?;
    if updater_feature && !d.may_self_update() {
        return Err(format!(
            "the `updater` feature must be off for DEMIOS_DISTRIBUTION={}: the store updates this build",
            value.unwrap_or_default().trim()
        ));
    }
    Ok(d)
}
