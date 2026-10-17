//! The notification menu.

use super::*;

/// A single notification (bell inbox).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notification {
    pub text: String,
    pub sent_time: Option<String>,
    /// The video the notification links to, if any.
    pub video_id: Option<String>,
    pub seen: bool,
}

/// Parse a `notification/get_notification_menu` response into notifications.
pub(crate) fn parse_notifications(v: &Value) -> Vec<Notification> {
    let mut out = Vec::new();
    walk(v, &mut |node| {
        if let Some(n) = node.get("notificationRenderer") {
            out.push(Notification {
                text: text(&n["shortMessage"]).unwrap_or_default(),
                sent_time: text(&n["sentTimeText"]),
                video_id: find_str(&n["navigationEndpoint"], "videoId"),
                seen: n["read"].as_bool().unwrap_or(false),
            });
        }
    });
    out
}
