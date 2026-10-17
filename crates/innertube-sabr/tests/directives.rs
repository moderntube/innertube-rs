use innertube_sabr::{Directive, UmpPart};

#[test]
fn sabr_redirect_extracts_the_new_url() {
    // SABR_REDIRECT (43), field 1 (string) is the URL to switch to. The URL is synthetic
    // because captured ones carry signed, IP-bound values.
    let url = "https://rr4.googlevideo.com/videoplayback?x=1";
    let mut payload = vec![0x0A, url.len() as u8]; // field 1, wire type 2
    payload.extend_from_slice(url.as_bytes());
    let part = UmpPart { part_type: 43, payload: payload.into() };

    assert_eq!(
        Directive::from_part(&part),
        Some(Directive::SabrRedirect { url: url.to_string() })
    );
}
