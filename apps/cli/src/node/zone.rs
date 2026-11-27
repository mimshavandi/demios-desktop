//! The Node's time zone, for jobs at a time of day (#257).
//!
//! - `node.json` names an IANA time zone (`"time_zone": "Europe/Berlin"`).
//! - `dn node init` writes the system's zone unless `--time-zone` names one.
//! - The zone itself and its daylight-saving rules are [`dn_jobs::zone::Zone`], shared
//!   with the app.

pub use dn_jobs::zone::Zone;
use std::ffi::OsString;
use std::path::Path;

/// The system's time zone name: `TZ` when set, otherwise where `/etc/localtime` points
/// (Unix). Windows asks the system.
pub fn system_name(
    env: &dyn Fn(&str) -> Option<OsString>,
    localtime: &Path,
) -> Result<String, String> {
    let from_path = |p: &str| p.split_once("zoneinfo/").map(|(_, n)| n.to_owned());
    if let Some(tz) = env("TZ").filter(|v| !v.is_empty()) {
        let tz = tz.to_string_lossy();
        let tz = tz.strip_prefix(':').unwrap_or(&tz);
        let name = if tz.starts_with('/') {
            from_path(tz).ok_or_else(|| format!("TZ is `{tz}`, which doesn't name a zone"))?
        } else {
            tz.to_owned()
        };
        Zone::named(&name)?;
        return Ok(name);
    }
    #[cfg(windows)]
    {
        let _ = localtime;
        Zone::system()
            .ok()
            .and_then(|z| z.name().map(str::to_owned))
            .ok_or_else(|| "couldn't read the system time zone; pass --time-zone".to_owned())
    }
    #[cfg(not(windows))]
    {
        let target = std::fs::read_link(localtime).map_err(|_| {
            format!(
                "couldn't tell the system time zone ({} isn't a link to a zone); pass \
                 --time-zone, e.g. --time-zone Europe/Berlin",
                localtime.display()
            )
        })?;
        let name = from_path(&target.to_string_lossy()).ok_or_else(|| {
            format!(
                "{} points to {}, which doesn't name a zone; pass --time-zone",
                localtime.display(),
                target.display()
            )
        })?;
        Zone::named(&name)?;
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_zone_comes_from_tz_or_else_etc_localtime() {
        let dir = std::env::temp_dir().join(format!("dn-zone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            let none = |_: &str| None::<OsString>;
            let link = dir.join("localtime");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink("/usr/share/zoneinfo/America/New_York", &link).unwrap();
            // TZ unset: /etc/localtime decides.
            assert_eq!(system_name(&none, &link).unwrap(), "America/New_York");
            // A copied file instead of a link can't be named.
            let copy = dir.join("copied");
            std::fs::write(&copy, b"TZif").unwrap();
            assert!(
                system_name(&none, &copy)
                    .unwrap_err()
                    .contains("--time-zone")
            );
        }
        // TZ set: it wins, with or without the leading colon or a zoneinfo path.
        let tz = |v: &'static str| move |k: &str| (k == "TZ").then(|| OsString::from(v));
        let missing = dir.join("missing");
        assert_eq!(
            system_name(&tz("Europe/Berlin"), &missing).unwrap(),
            "Europe/Berlin"
        );
        assert_eq!(
            system_name(&tz(":Asia/Tokyo"), &missing).unwrap(),
            "Asia/Tokyo"
        );
        assert_eq!(
            system_name(&tz("/usr/share/zoneinfo/Europe/Paris"), &missing).unwrap(),
            "Europe/Paris"
        );
        assert!(system_name(&tz("Mars/Olympus"), &missing).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
