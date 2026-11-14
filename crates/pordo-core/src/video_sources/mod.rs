//! Video discovery and account protocols shared by native hosts.
pub mod bilibili;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub source: String,
    pub check_updates: bool,
    pub speed: u16,
    pub danmaku: DanmakuSettings,
    pub skip: SkipSettings,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            source: "bilibili".into(),
            check_updates: true,
            speed: 100,
            danmaku: Default::default(),
            skip: Default::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SkipSettings {
    pub intro: bool,
    pub outro: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DanmakuSettings {
    pub enabled: bool,
    /// Percentage of the picture height, measured from its top edge.
    pub area: u8,
    pub opacity: u8,
    pub font: u16,
    pub speed: u16,
}
impl Default for DanmakuSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            area: 50,
            opacity: 80,
            font: 100,
            speed: 100,
        }
    }
}
