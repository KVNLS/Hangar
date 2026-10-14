//! Dumps the private simulator API surface for a booted simulator.
//! `cargo run -p hangar-examples --example sim_probe -- [UDID]`

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    let udid = match std::env::args().nth(1) {
        Some(u) => u,
        None => hangar_devices::simctl::list_booted()?
            .into_iter()
            .next()
            .map(|d| d.udid)
            .ok_or_else(|| anyhow::anyhow!("no booted simulator"))?,
    };
    println!("{}", hangar_devices::ios::introspect(&udid)?);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("sim_probe requires macOS");
}
