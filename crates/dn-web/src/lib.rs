//! dn-web: web search and page fetching for the engine (#369).
//!
//! Every request goes from the user's device or Node, with the user's own keys; there is no
//! Demios proxy.
//!
//! - [`guard`]: which addresses a fetch may reach. Loopback, private, link-local (cloud
//!   metadata), CGNAT and other non-public ranges are refused, for IPv4 and IPv6, at DNS
//!   resolution time, so the client connects only to addresses that were checked.
//! - [`fetch`]: the [`Fetcher`]: an honest user agent, redirects re-checked
//!   hop by hop, time and size caps, allowed content types, and a per-host rate limit.
//!
//! Extraction to markdown and robots.txt come with their dependencies (#369); search
//! providers next.

pub mod fetch;
pub mod guard;
mod rate;

pub use fetch::{FetchError, Fetched, Fetcher, FetcherConfig};
