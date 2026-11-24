//! `demios://models/add?…` links (T-INF-12, issue #48; format in `dn_models::link`).
//!
//! The OS hands a link to the app (deep-link plugin; a second launch forwards it to the
//! running app through the single-instance plugin). A link is untrusted data: it goes only
//! through `AddModelLink::parse`, never into command-line handling, and it only *opens the
//! add-model confirmation screen*. Nothing downloads until the user clicks Download there.
//! While a confirmation from a link is open, further links are ignored, so a page can't swap
//! the model under the user's cursor.

use dn_models::link::{AddModelLink, LinkError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Delivers a link to the screen; returns false when it couldn't (e.g. the webview reloaded
/// and its channel is gone).
pub type LinkSink = Arc<dyn Fn(AddModelLink) -> bool + Send + Sync>;

/// Why a link was not shown.
#[derive(Debug, PartialEq, Eq)]
pub enum Ignored {
    /// A confirmation from an earlier link is still open.
    Busy,
    Invalid(LinkError),
}

#[derive(Default)]
pub struct AddLinks {
    /// Received before the screen subscribed (the app was started by the link).
    pending: Mutex<Option<AddModelLink>>,
    /// A link's confirmation is open (or waiting to be shown).
    open: AtomicBool,
    sink: Mutex<Option<LinkSink>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl AddLinks {
    /// A URL the OS handed over. Shown on the confirmation screen if it is a valid
    /// add-model link and no other link's confirmation is open.
    pub fn receive(&self, url: &str) -> Result<(), Ignored> {
        let link = AddModelLink::parse(url).map_err(Ignored::Invalid)?;
        if self
            .open
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(Ignored::Busy);
        }
        let sink = lock(&self.sink).clone();
        let delivered = sink.is_some_and(|s| s(link.clone()));
        if !delivered {
            // Nobody (alive) to show it: keep it for the next subscriber.
            *lock(&self.sink) = None;
            *lock(&self.pending) = Some(link);
        }
        Ok(())
    }

    /// The screen subscribes (at start, or again after the webview reloaded). Returns a link
    /// that arrived before it did, or that the old screen never got. With nothing to show, a
    /// confirmation the old screen had open is gone with it, so the next link may open.
    pub fn watch(&self, sink: LinkSink) -> Option<AddModelLink> {
        *lock(&self.sink) = Some(sink);
        let pending = lock(&self.pending).take();
        if pending.is_none() {
            self.open.store(false, Ordering::SeqCst);
        }
        pending
    }

    /// The link's confirmation was closed (downloaded or dismissed): the next link may open.
    pub fn done(&self) {
        lock(&self.pending).take();
        self.open.store(false, Ordering::SeqCst);
    }
}

/// Re-checks a link the webview sends back: it must be exactly what `parse` accepts, so the
/// webview can't widen what a link asks for.
pub fn revalidate(link: &AddModelLink) -> Result<AddModelLink, LinkError> {
    let again = AddModelLink::parse(&link.to_url())?;
    if &again == link {
        Ok(again)
    } else {
        Err(LinkError::NotAnAddLink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collecting() -> (AddLinks, Arc<Mutex<Vec<AddModelLink>>>) {
        let links = AddLinks::default();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = Arc::clone(&got);
        assert!(
            links
                .watch(Arc::new(move |l| {
                    lock(&g2).push(l);
                    true
                }))
                .is_none()
        );
        (links, got)
    }

    #[test]
    fn a_valid_link_opens_the_screen_and_others_wait_until_it_closes() {
        let (links, got) = collecting();
        links
            .receive("demios://models/add?repo=org/Tiny-GGUF&quant=Q4_K_M")
            .unwrap();
        assert_eq!(lock(&got)[0].repo, "org/Tiny-GGUF");
        assert_eq!(lock(&got)[0].quant.as_deref(), Some("Q4_K_M"));
        // A second link while the confirmation is open is ignored, not queued.
        assert_eq!(
            links.receive("demios://models/add?repo=evil/Other-GGUF"),
            Err(Ignored::Busy)
        );
        assert_eq!(lock(&got).len(), 1);
        links.done();
        links
            .receive("demios://models/add?repo=org/Next-GGUF")
            .unwrap();
        assert_eq!(lock(&got).len(), 2);
    }

    #[test]
    fn a_link_that_started_the_app_waits_for_the_screen() {
        let links = AddLinks::default();
        links
            .receive("demios://models/add?repo=org/Tiny-GGUF")
            .unwrap();
        let pending = links.watch(Arc::new(|_| true)).unwrap();
        assert_eq!(pending.repo, "org/Tiny-GGUF");
        assert!(links.watch(Arc::new(|_| true)).is_none(), "delivered once");
    }

    #[test]
    fn malformed_and_hostile_links_are_ignored() {
        let (links, got) = collecting();
        for bad in [
            "",
            "demios://",
            "demios://models/remove?repo=org/x",
            "demios://models/add",
            "demios://models/add?repo=",
            "demios://models/add?repo=../../etc",
            "demios://models/add?repo=org/x&repo=org/y",
            "demios://models/add?repo=org/x&file=a.gguf&quant=Q4_K_M",
            "demios://models/add?repo=org/x&file=../../.ssh/id_rsa",
            "demios://models/add?repo=org/x&download=1",
            "demios://models/add?repo=org/x&autostart=true",
            "demios://models/add/../../settings?repo=org/x",
            "https://evil.example/models/add?repo=org/x",
            "javascript:alert(1)",
            "--inspect=0.0.0.0:9229",
            "demios://models/add?repo=org%2Fx%0A--flag",
        ] {
            assert!(
                matches!(links.receive(bad), Err(Ignored::Invalid(_))),
                "{bad:?}"
            );
        }
        let long = format!("demios://models/add?repo=org/x&utm_x={}", "a".repeat(3000));
        assert!(matches!(links.receive(&long), Err(Ignored::Invalid(_))));
        assert!(lock(&got).is_empty());
        // Invalid links never block the next valid one.
        links
            .receive("demios://models/add?repo=org/Tiny-GGUF")
            .unwrap();
    }

    #[test]
    fn a_link_the_webview_couldnt_receive_waits_for_the_next_screen() {
        let links = AddLinks::default();
        // The webview reloaded: its channel is gone, sending fails.
        assert!(links.watch(Arc::new(|_| false)).is_none());
        links
            .receive("demios://models/add?repo=org/Tiny-GGUF")
            .unwrap();
        // The reloaded screen subscribes and gets it.
        let again = links.watch(Arc::new(|_| true)).unwrap();
        assert_eq!(again.repo, "org/Tiny-GGUF");
    }

    #[test]
    fn a_reloaded_screen_reopens_the_gate_for_new_links() {
        let links = AddLinks::default();
        links.watch(Arc::new(|_| true));
        links
            .receive("demios://models/add?repo=org/Tiny-GGUF")
            .unwrap();
        assert_eq!(
            links.receive("demios://models/add?repo=org/Other-GGUF"),
            Err(Ignored::Busy)
        );
        // The webview reloads mid-confirmation (never calls done): its new screen subscribes.
        assert!(links.watch(Arc::new(|_| true)).is_none());
        links
            .receive("demios://models/add?repo=org/Other-GGUF")
            .expect("not blocked by the confirmation that went away");
    }

    #[test]
    fn every_accepted_link_round_trips_exactly() {
        // revalidate's equality check guards against a future parser that normalises values;
        // with today's parser every accepted link comes back identical.
        for url in [
            "demios://models/add?repo=org/Tiny-GGUF",
            "demios://models/add?repo=org/Tiny-GGUF&quant=Q4_K_M&revision=main",
            "demios://models/add?repo=org/Tiny-GGUF&file=sub/dir/m%C3%B3del-00001-of-00002.gguf",
            "demios://models/add?repo=Org.Name/Model_v1.5-GGUF&revision=refs/pr/12",
            "demios://models/add?repo=org/Tiny-GGUF&quant=iq3_xxs&utm_source=docs",
        ] {
            let l = AddModelLink::parse(url).unwrap();
            assert_eq!(revalidate(&l).unwrap(), l, "{url}");
        }
    }

    #[test]
    fn the_webview_cant_widen_a_link() {
        let ok =
            AddModelLink::parse("demios://models/add?repo=org/Tiny-GGUF&quant=Q4_K_M").unwrap();
        assert_eq!(revalidate(&ok).unwrap(), ok);
        let bad = AddModelLink {
            repo: "org/Tiny-GGUF".into(),
            file: Some("../../secret".into()),
            quant: None,
            revision: None,
        };
        assert!(revalidate(&bad).is_err());
    }
}
