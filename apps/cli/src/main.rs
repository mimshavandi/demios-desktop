//! `dn` — headless entry point for running models, skills and benchmarks (T-INF-*).

fn main() {
    println!(
        "dn {} — commands arrive with T-INF-01",
        env!("CARGO_PKG_VERSION")
    );
}
