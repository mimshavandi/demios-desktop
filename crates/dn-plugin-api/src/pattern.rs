//! Capability-id patterns: an exact id (`mcp.files.read`) or a prefix ending in `*`
//! (`mcp.files.*`). The same syntax as a skill's `allowed-tools`.

use crate::error::PluginError;

/// Checks a pattern: lowercase letters, digits, `.`, `_` and `-`, with an optional trailing
/// `*`, at most 200 bytes. A lone `*` is refused: an add-on lists what it calls.
pub fn check_pattern(p: &str) -> Result<(), PluginError> {
    let body = p.strip_suffix('*').unwrap_or(p);
    let ok = !body.is_empty()
        && p.len() <= 200
        && body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(PluginError::invalid(format!(
            "`{p}` isn't a tool pattern: use a tool id such as `mcp.files.read`, or a prefix \
             ending in `*` such as `mcp.files.*`"
        )))
    }
}

/// True when `capability_id` matches one of `patterns`. An empty list matches nothing.
pub fn matches_any<S: AsRef<str>>(patterns: &[S], capability_id: &str) -> bool {
    patterns.iter().any(|p| {
        let p = p.as_ref();
        match p.strip_suffix('*') {
            Some(prefix) => capability_id.starts_with(prefix),
            None => p == capability_id,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_ids_and_prefixes_match() {
        let p = ["mcp.files.*", "artifact.create"];
        assert!(matches_any(&p, "mcp.files.read"));
        assert!(matches_any(&p, "artifact.create"));
        assert!(!matches_any(&p, "artifact.update"));
        assert!(!matches_any(&p, "mcp.filesystem.read"));
        assert!(!matches_any::<&str>(&[], "artifact.create"));
    }

    #[test]
    fn a_lone_star_and_odd_characters_are_refused() {
        for bad in ["*", "", "mcp.*.read", "Mcp.x", "a b"] {
            assert!(check_pattern(bad).is_err(), "{bad:?}");
        }
    }
}
