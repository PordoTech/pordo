//! Video discovery and account protocols shared by native hosts.
pub mod bilibili;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub source: String,
    pub danmaku: DanmakuSettings,
    #[serde(deserialize_with = "deserialize_skip")]
    pub skip: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            source: "bilibili".into(),
            danmaku: Default::default(),
            skip: true,
        }
    }
}

fn deserialize_skip<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Saved {
        Unified(bool),
        Separate {
            #[serde(default)]
            intro: bool,
            #[serde(default)]
            outro: bool,
        },
    }
    Ok(
        match <Saved as serde::Deserialize>::deserialize(deserializer)? {
            Saved::Unified(enabled) => enabled,
            // Keep the feature enabled for anyone using either of the old switches.
            Saved::Separate { intro, outro } => intro || outro,
        },
    )
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
