//! Mint one token with `NativeProvider` from a saved page attestation.
//!
//!   native_mint <dir with challenge.json + ytcfg.json> <content_binding>
use innertube_pot::{NativeProvider, PageAttestation, PoTokenProvider};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = &args[1];
    let attest = PageAttestation {
        challenge: std::fs::read_to_string(format!("{dir}/challenge.json")).expect("challenge.json"),
        yt_config: std::fs::read_to_string(format!("{dir}/ytcfg.json")).expect("ytcfg.json"),
    };
    let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
    let p = NativeProvider::new(ua);
    match p.get_po_token(&args[2], Some(&attest)).await {
        Ok(t) => println!("pot {} chars", t.len()),
        Err(e) => println!("mint failed: {e}"),
    }
}
