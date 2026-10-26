//! Channel names, display colours and display windows, from the header.

use crate::error::SisError;
use crate::reader::{attr, find, num};

/// One fluorescence channel, as recorded in the header.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub name: String,
    /// Display color, from the end of the channel's color transfer function.
    pub color_rgb: [u8; 3],
    /// Display range (min, max), when the file records one.
    pub window: Option<(f64, f64)>,
}

/// Channel colors when the file records none.
const PALETTE: [[u8; 3]; 6] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [255, 255, 0],
    [255, 0, 255],
    [0, 255, 255],
];

/// The channels of `scope`, sorted by index. Display settings live in a JSON attribute of
/// `root`'s `<ColorSettings>`, keyed by scope name and channel id; when they are missing
/// or unreadable a channel gets a palette colour, no window, and "Channel N" if unnamed.
pub(crate) fn read_channels(
    root: roxmltree::Node<'_, '_>,
    scope: roxmltree::Node<'_, '_>,
) -> Result<Vec<ChannelInfo>, SisError> {
    let settings: Vec<serde_json::Value> = find(root, "ColorSettings")
        .ok()
        .and_then(|n| n.attribute("value"))
        .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
        .and_then(|v| {
            let scope_name = scope.attribute("name")?;
            v["Scopes"]
                .as_array()?
                .iter()
                .find(|s| s["Id"].as_str() == Some(scope_name))?["ChannelSettings"]
                .as_array()
                .cloned()
        })
        .unwrap_or_default();
    let mut channels: Vec<(u64, ChannelInfo)> = find(scope, "Channels")?
        .children()
        .filter(|n| n.has_tag_name("Channel"))
        .map(|ch| {
            let id = num(ch, "id")?;
            let s = settings.iter().find(|s| s["Id"].as_f64() == Some(id));
            let index = num(ch, "index")? as u64;
            let fallback = PALETTE[index as usize % PALETTE.len()];
            let end = |key: &str, i: usize| {
                s.and_then(|s| s[key].as_array()?.last()?.as_u64())
                    .map_or(fallback[i], |v| v.min(255) as u8)
            };
            let name = attr(ch, "name")?;
            let window = s.and_then(|s| Some((s["RangeMin"].as_f64()?, s["RangeMax"].as_f64()?)));
            Ok((
                index,
                ChannelInfo {
                    name: if name.is_empty() {
                        format!("Channel {}", index + 1)
                    } else {
                        name
                    },
                    color_rgb: [end("CtfR", 0), end("CtfG", 1), end("CtfB", 2)],
                    window,
                },
            ))
        })
        .collect::<Result<_, SisError>>()?;
    channels.sort_by_key(|(i, _)| *i);
    Ok(channels.into_iter().map(|(_, c)| c).collect())
}
