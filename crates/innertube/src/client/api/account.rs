//! The signed-in account menu.

use super::*;

/// The signed-in account's identity, parsed from `account/account_menu`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountInfo {
    pub name: Option<String>,
    /// `@handle`, when present.
    pub handle: Option<String>,
    /// Largest avatar thumbnail URL.
    pub avatar_url: Option<String>,
}

/// Parse the `account/account_menu` response into the signed-in identity. Paths verified
/// against ytmusicapi + multiple InnerTube clients (`activeAccountHeaderRenderer`).
pub(crate) fn parse_account_info(v: &Value) -> AccountInfo {
    let h = &v["actions"][0]["openPopupAction"]["popup"]["multiPageMenuRenderer"]["header"]
        ["activeAccountHeaderRenderer"];
    AccountInfo {
        name: text(&h["accountName"]),
        handle: text(&h["channelHandle"]),
        avatar_url: last_thumb(&h["accountPhoto"]),
    }
}
