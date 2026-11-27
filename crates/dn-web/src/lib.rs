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
//! - [`extract`]: a page's main text as markdown (Readability, through `dom_smoothie`).
//!
//! - [`robots`]: robots.txt as RFC 9309 says, honoured per site by the fetcher.
//!
//! - [`search`]: [`search::SearchProvider`] and its backends: Brave, Linkup, Exa and Tavily with
//!   the user's key, and SearXNG at the user's URL.

pub mod extract;
pub mod fetch;
pub mod guard;
mod rate;
pub mod robots;
pub mod search;

pub use extract::Page;
pub use fetch::{FetchError, Fetched, Fetcher, FetcherConfig};
