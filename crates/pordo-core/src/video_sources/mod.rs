//! Video discovery and account protocols shared by native hosts.
pub mod bilibili;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub source: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            source: "bilibili".into(),
        }
    }
}
