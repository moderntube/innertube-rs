use innertube::{ClientConfig, Credentials, Error, Session};

/// Write and account methods fail with `NotLoggedIn` on an anonymous session, before any request.
#[tokio::test]
async fn write_methods_require_auth() {
    let session = Session::new(ClientConfig::web()).unwrap();
    assert!(!session.is_authenticated());

    assert!(matches!(session.like("dQw4w9WgXcQ").await, Err(Error::NotLoggedIn)));
    assert!(matches!(session.subscribe("UCfoo").await, Err(Error::NotLoggedIn)));
    assert!(matches!(session.delete_playlist("PLfoo").await, Err(Error::NotLoggedIn)));
    assert!(matches!(session.notifications().await, Err(Error::NotLoggedIn)));
    assert!(matches!(session.comment("dQw4w9WgXcQ", "hi").await, Err(Error::NotLoggedIn)));
}

#[test]
fn authenticated_session_reports_signed_in() {
    let creds = Credentials::from_cookie_header("SAPISID=TEST_SAPISID_VALUE").unwrap();
    let session = Session::authenticated(ClientConfig::web(), creds).unwrap();
    assert!(session.is_authenticated());
}
