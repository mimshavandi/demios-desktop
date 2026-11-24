//! `demios://models/add?…` links (ADR-0011): a website, doc or shared recommendation points at
//! a Hugging Face GGUF; the app opens its add-model flow on a **confirmation screen**.
//!
//! This module only parses, builds and resolves links. Nothing here downloads: a link is
//! untrusted input, so [`AddModelLink::resolve`] returns an [`AddModelPlan`] (model, size,
//! licence, warnings) for the user to confirm, and the download starts only from that click
//! (`HubClient::pull` with the plan's `info` and `variant`).
//!
//! Format: `demios://models/add?repo=<owner/name>[&file=<path>|&quant=<Q4_K_M>][&revision=<rev>]`
//! - `repo` is required. `file` (a repo path, any part of a split GGUF) or `quant` picks
//!   the variant; neither means the user picks one on the screen.
//! - `revision` defaults to `main` and is always resolved to a commit before showing.
//! - Unknown parameters are ignored (forward compatibility); duplicates, an over-long link,
//!   or any other scheme, host or path are rejected.

use crate::hub::{
    GgufVariant, HubClient, HubError, LicenseWarning, RepoInfo, VariantKind, validate_repo_id,
    validate_repo_path,
};
use serde::{Deserialize, Serialize};

pub const SCHEME: &str = "demios";
const MAX_LEN: usize = 2048;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LinkError {
    #[error("not a demios://models/add link")]
    NotAnAddLink,
    #[error("link is too long")]
    TooLong,
    #[error("missing `{0}`")]
    Missing(&'static str),
    #[error("`{0}` given more than once")]
    Duplicate(String),
    #[error("give `file` or `quant`, not both")]
    FileAndQuant,
    #[error("invalid `{param}`: {value}")]
    Invalid { param: &'static str, value: String },
}

/// What a link asks for (validated, not yet resolved).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddModelLink {
    pub repo: String,
    pub file: Option<String>,
    pub quant: Option<String>,
    pub revision: Option<String>,
}

impl AddModelLink {
    pub fn parse(link: &str) -> Result<Self, LinkError> {
        if link.len() > MAX_LEN {
            return Err(LinkError::TooLong);
        }
        let u = reqwest::Url::parse(link.trim()).map_err(|_| LinkError::NotAnAddLink)?;
        if u.scheme() != SCHEME
            || u.host_str() != Some("models")
            || u.path().trim_end_matches('/') != "/add"
            || !u.username().is_empty()
            || u.password().is_some()
            || u.port().is_some()
            || u.fragment().is_some()
        {
            return Err(LinkError::NotAnAddLink);
        }
        let (mut repo, mut file, mut quant, mut revision) = (None, None, None, None);
        for (k, v) in u.query_pairs() {
            let slot = match k.as_ref() {
                "repo" => &mut repo,
                "file" => &mut file,
                "quant" => &mut quant,
                "revision" => &mut revision,
                _ => continue,
            };
            if slot.is_some() {
                return Err(LinkError::Duplicate(k.into_owned()));
            }
            *slot = Some(v.into_owned());
        }
        let repo = repo.ok_or(LinkError::Missing("repo"))?;
        validate_repo_id(&repo).map_err(|_| LinkError::Invalid {
            param: "repo",
            value: repo.clone(),
        })?;
        if file.is_some() && quant.is_some() {
            return Err(LinkError::FileAndQuant);
        }
        if let Some(f) = &file
            && (validate_repo_path(f).is_err() || !f.to_ascii_lowercase().ends_with(".gguf"))
        {
            return Err(LinkError::Invalid {
                param: "file",
                value: f.clone(),
            });
        }
        if let Some(q) = &quant
            && (q.is_empty()
                || q.len() > 32
                || !q.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        {
            return Err(LinkError::Invalid {
                param: "quant",
                value: q.clone(),
            });
        }
        if let Some(r) = &revision {
            let ok = !r.is_empty()
                && r.len() <= 255
                && !r.contains("..")
                && r.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
            if !ok {
                return Err(LinkError::Invalid {
                    param: "revision",
                    value: r.clone(),
                });
            }
        }
        Ok(Self {
            repo,
            file,
            quant,
            revision,
        })
    }

    /// The link for this request (for the website, docs, or "share this model").
    pub fn to_url(&self) -> String {
        let mut u = reqwest::Url::parse("demios://models/add").expect("static url");
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("repo", &self.repo);
            if let Some(f) = &self.file {
                q.append_pair("file", f);
            }
            if let Some(x) = &self.quant {
                q.append_pair("quant", x);
            }
            if let Some(r) = &self.revision {
                q.append_pair("revision", r);
            }
        }
        u.to_string()
    }

    /// Look the link up on the Hub (pin the commit, list variants) without downloading.
    pub async fn resolve(&self, hub: &HubClient) -> Result<AddModelPlan, HubError> {
        let info = hub.repo(&self.repo, self.revision.as_deref()).await?;
        let variants = hub.variants(&self.repo, &info.sha).await?;
        let selected = if let Some(f) = &self.file {
            let i = variants
                .iter()
                .position(|v| v.files.iter().any(|x| &x.path == f))
                .ok_or_else(|| HubError::NotFound(format!("{}: {f}", self.repo)))?;
            Some(i)
        } else if let Some(q) = &self.quant {
            let hits: Vec<usize> = variants
                .iter()
                .enumerate()
                .filter(|(_, v)| {
                    v.kind == VariantKind::Model
                        && v.quant
                            .as_deref()
                            .is_some_and(|x| x.eq_ignore_ascii_case(q))
                })
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [] => return Err(HubError::NotFound(format!("{}: quant {q}", self.repo))),
                // Several (e.g. UD- and plain): let the user pick on the screen.
                [i] => Some(*i),
                _ => None,
            }
        } else {
            None
        };
        let mut warnings = vec![];
        if let Some(w) = info.license.warning {
            warnings.push(PlanWarning::License(w));
        }
        if info.gated {
            warnings.push(PlanWarning::Gated);
        }
        if let Some(i) = selected
            && variants[i].files.iter().any(|f| f.sha256.is_none())
        {
            warnings.push(PlanWarning::Unverifiable);
        }
        Ok(AddModelPlan {
            link: self.clone(),
            info,
            variants,
            selected,
            warnings,
        })
    }
}

/// Things the confirmation screen must show next to the download button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum PlanWarning {
    License(LicenseWarning),
    /// Needs the user's Hugging Face token and accepted terms.
    Gated,
    /// The selected file has no LFS sha256: the download button must stay disabled.
    Unverifiable,
}

/// What the confirmation screen shows. Downloading is a separate, user-triggered step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AddModelPlan {
    pub link: AddModelLink,
    /// Repo pinned to a commit, with licence and card link.
    pub info: RepoInfo,
    pub variants: Vec<GgufVariant>,
    /// Index into `variants` the link chose, if it chose exactly one.
    pub selected: Option<usize>,
    pub warnings: Vec<PlanWarning>,
}

impl AddModelPlan {
    pub fn selected_variant(&self) -> Option<&GgufVariant> {
        self.selected.and_then(|i| self.variants.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_round_trips() {
        let l = AddModelLink::parse(
            "demios://models/add?repo=Qwen/Qwen3-8B-GGUF&quant=Q4_K_M&revision=main&utm=x",
        )
        .unwrap();
        assert_eq!(l.repo, "Qwen/Qwen3-8B-GGUF");
        assert_eq!(l.quant.as_deref(), Some("Q4_K_M"));
        assert_eq!(l.revision.as_deref(), Some("main"));
        assert_eq!(AddModelLink::parse(&l.to_url()).unwrap(), l);
        let f = AddModelLink {
            repo: "o/n".into(),
            file: Some("Q8_0/m-00001-of-00002.gguf".into()),
            quant: None,
            revision: None,
        };
        assert_eq!(AddModelLink::parse(&f.to_url()).unwrap(), f);
    }

    #[tokio::test]
    async fn resolve_plans_without_downloading() {
        use crate::hub::HubConfig;
        use crate::testserver::{Reply, serve};
        use std::collections::HashMap;
        const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
        let rev = format!(
            r#"{{"id":"o/n","sha":"{SHA}","gated":"auto","cardData":{{"license":"cc-by-nc-4.0"}}}}"#
        );
        let h = "a".repeat(64);
        let tree = format!(
            r#"[{{"type":"file","path":"m-Q4_K_M.gguf","size":10,"lfs":{{"oid":"{h}","size":10}}}},
                {{"type":"file","path":"m-UD-Q8_0.gguf","size":20,"lfs":{{"oid":"{h}","size":20}}}},
                {{"type":"file","path":"m-Q8_0.gguf","size":19,"lfs":{{"oid":"{h}","size":19}}}}]"#
        );
        let mut routes = HashMap::new();
        routes.insert(
            "/api/models/o/n/revision/main".to_string(),
            Reply::json(&rev),
        );
        routes.insert(
            format!("/api/models/o/n/tree/{SHA}?recursive=true"),
            Reply::json(&tree),
        );
        let s = serve(routes).await;
        let hub = HubClient::new(HubConfig {
            endpoint: Some(s.url.clone()),
            token: None,
        })
        .unwrap();
        let plan = AddModelLink::parse("demios://models/add?repo=o/n&quant=q4_k_m")
            .unwrap()
            .resolve(&hub)
            .await
            .unwrap();
        assert_eq!(plan.info.sha, SHA);
        assert_eq!(plan.selected_variant().unwrap().name, "m-Q4_K_M");
        assert_eq!(
            plan.warnings,
            [
                PlanWarning::License(LicenseWarning::NonCommercial),
                PlanWarning::Gated
            ]
        );
        // Two Q8_0 variants: the user chooses on the screen.
        let plan = AddModelLink::parse("demios://models/add?repo=o/n&quant=Q8_0")
            .unwrap()
            .resolve(&hub)
            .await
            .unwrap();
        assert_eq!(plan.selected, None);
        let by_file = AddModelLink::parse("demios://models/add?repo=o/n&file=m-Q8_0.gguf")
            .unwrap()
            .resolve(&hub)
            .await
            .unwrap();
        assert_eq!(by_file.selected_variant().unwrap().name, "m-Q8_0");
        // Only metadata was requested: no /resolve/ (file download) calls.
        assert!(
            s.requests
                .lock()
                .unwrap()
                .iter()
                .all(|r| !r.contains("/resolve/"))
        );
    }

    #[test]
    fn rejects_bad_links() {
        use LinkError::*;
        let e = |s: &str| AddModelLink::parse(s).unwrap_err();
        assert_eq!(e("https://models/add?repo=o/n"), NotAnAddLink);
        assert_eq!(e("demios://models/remove?repo=o/n"), NotAnAddLink);
        assert_eq!(e("demios://evil/add?repo=o/n"), NotAnAddLink);
        assert_eq!(e("demios://u:p@models/add?repo=o/n"), NotAnAddLink);
        assert_eq!(e("demios://models/add"), Missing("repo"));
        assert_eq!(
            e("demios://models/add?repo=o/n&repo=x/y"),
            Duplicate("repo".into())
        );
        assert_eq!(
            e("demios://models/add?repo=o/n&file=m.gguf&quant=Q8_0"),
            FileAndQuant
        );
        assert!(matches!(
            e("demios://models/add?repo=../etc"),
            Invalid { param: "repo", .. }
        ));
        assert!(matches!(
            e("demios://models/add?repo=o/n&file=../../x.gguf"),
            Invalid { param: "file", .. }
        ));
        assert!(matches!(
            e("demios://models/add?repo=o/n&file=run.sh"),
            Invalid { param: "file", .. }
        ));
        assert!(matches!(
            e("demios://models/add?repo=o/n&quant=Q4;rm"),
            Invalid { param: "quant", .. }
        ));
        assert!(matches!(
            e("demios://models/add?repo=o/n&revision=a..b"),
            Invalid {
                param: "revision",
                ..
            }
        ));
        assert_eq!(
            e(&format!(
                "demios://models/add?repo=o/n&x={}",
                "a".repeat(3000)
            )),
            TooLong
        );
    }
}
