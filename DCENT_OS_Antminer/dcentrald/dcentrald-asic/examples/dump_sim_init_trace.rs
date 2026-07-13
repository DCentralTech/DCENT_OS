#[cfg(not(feature = "sim-hal"))]
fn main() {
    eprintln!("enable --features sim-hal");
    std::process::exit(2);
}

#[cfg(feature = "sim-hal")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::str::FromStr;

    use dcentrald_asic::drivers::ChipRegistry;
    use dcentrald_hal::fpga_chain::FpgaChain;
    use dcentrald_hal::platform::sim::SimModel;

    let slug = std::env::args()
        .nth(1)
        .ok_or("usage: dump_sim_init_trace <model-slug>")?;
    let evidence = dcentrald_re_catalog::model_evidence(&slug)
        .ok_or_else(|| format!("no RE catalog row for {slug}"))?;
    let count = evidence
        .chips_per_chain
        .ok_or_else(|| format!("{slug} has no evidence-backed chip count"))?;
    let count = u8::try_from(count)?;
    let frequency = evidence
        .default_frequency_mhz
        .ok_or_else(|| format!("{slug} has no evidence-backed default frequency"))?;
    let model = SimModel::from_str(&slug)?;
    let registry = ChipRegistry::production();
    let driver = registry
        .detect(evidence.chip_id)
        .ok_or_else(|| format!("chip 0x{:04x} is not a production driver", evidence.chip_id))?;
    let mut chain = FpgaChain::open_sim_for_model(0, model)?;
    driver.init_chain(&mut chain, count, frequency)?;

    println!(
        "{}",
        serde_json::json!({
            "schema": "dcent-init-trace-v1",
            "model": evidence.slug,
            "strictness": match evidence.strength {
                dcentrald_re_catalog::EvidenceStrength::Exact => "exact",
                dcentrald_re_catalog::EvidenceStrength::Structural => "structural",
                dcentrald_re_catalog::EvidenceStrength::Scaffold => "scaffold",
            },
            "provenance": evidence.provenance,
        })
    );
    let trace = chain.drain_sim_trace()?;
    if std::env::args().any(|arg| arg == "--semantic") {
        let mut index = 0;
        while index < trace.len() {
            match &trace[index] {
                dcentrald_hal::platform::sim::TraceEvent::BaudChanged { baud, .. } => {
                    println!("baud {baud}");
                }
                dcentrald_hal::platform::sim::TraceEvent::Command { bytes, .. }
                    if bytes.len() == 4
                        && matches!(bytes[0], 0x41 | 0x48 | 0x51 | 0x58)
                        && index + 1 < trace.len() =>
                {
                    if let dcentrald_hal::platform::sim::TraceEvent::Command {
                        bytes: value, ..
                    } = &trace[index + 1]
                    {
                        if value.len() == 4 {
                            println!(
                                "write header=0x{:02x} addr=0x{:02x} reg=0x{:02x} value=0x{:08x}",
                                bytes[0],
                                bytes[2],
                                bytes[3],
                                u32::from_be_bytes([value[0], value[1], value[2], value[3]])
                            );
                            index += 1;
                        }
                    }
                }
                _ => {}
            }
            index += 1;
        }
    } else {
        for event in trace {
            println!("{}", serde_json::to_string(&event)?);
        }
    }
    Ok(())
}
