use serde::{Deserialize, Serialize};

/// Session data stored in an encrypted cookie.
///
/// Kept small (~500 bytes encrypted) to stay well under the 4KB browser
/// cookie size limit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub workspace_id: String,
    pub tier: String,
}

impl Session {
    /// First character of the user's name, uppercased, for avatar initials.
    pub fn initial(&self) -> String {
        self.name
            .chars()
            .next()
            .unwrap_or('?')
            .to_uppercase()
            .to_string()
    }
}
