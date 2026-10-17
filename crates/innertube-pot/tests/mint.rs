use innertube_pot::{BgUtilProvider, PoTokenProvider};

// Needs a bgutil server on :4416.
#[tokio::test]
#[ignore = "needs bgutil server + network"]
async fn mints_a_token_for_a_video() {
    let provider = BgUtilProvider::local();
    let token = provider.get_po_token("36YnV9STBqc", None).await.unwrap();
    assert!(token.len() > 20, "expected a real token, got {token:?}");
}
