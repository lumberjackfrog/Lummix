use regex::RegexSet;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

const MODELS: &[(&str, &str)] = &[
    (r"vive.*tracker.*3\.0", "VIVE Tracker 3.0"),
    (r"vive.*tracker.*2\.0", "VIVE Tracker 2.0"),
    (r"ultimate tracker", "VIVE Ultimate Tracker"),
    (r"vive.*tracker", "VIVE Tracker"),
    (r"tundra.*tracker", "Tundra Tracker"),
    (r"base station.*2|basestation.*2", "Base Station 2.0"),
    (r"base station|basestation", "Base Station"),
    (r"pico[ _-]*4[ _-]*ultra", "PICO 4 Ultra"),
    (
        r"pico[ _-]*4[ _-]*(enterprise|pro)",
        "PICO 4 Enterprise/Pro",
    ),
    (r"pico[ _-]*4", "PICO 4"),
    (r"pico.*neo[ _-]*3", "PICO Neo 3"),
    (r"quest[ _-]*3s", "Meta Quest 3S"),
    (r"quest[ _-]*3", "Meta Quest 3"),
    (r"quest[ _-]*2", "Meta Quest 2"),
    (r"quest[ _-]*pro", "Meta Quest Pro"),
    (r"(?:oculus|meta)[ _-]*quest", "Meta Quest"),
    (r"rift[ _-]*s", "Oculus Rift S"),
    (r"oculus[ _-]*rift", "Oculus Rift"),
    (r"index.*controller|knuckles", "Valve Index Controller"),
    (r"valve[ _-]*index|index[ _-]*hmd", "Valve Index"),
    (r"vive[ _-]*pro[ _-]*2", "VIVE Pro 2"),
    (r"vive[ _-]*pro", "VIVE Pro"),
    (r"vive.*focus", "VIVE Focus"),
    (r"vive.*cosmos", "VIVE Cosmos"),
    (r"vive[ _-]*controller", "VIVE Controller"),
    (r"vive[ _-]*hmd", "HTC VIVE"),
    (r"bigscreen[ _-]*beyond", "Bigscreen Beyond"),
    (r"reverb[ _-]*g2", "HP Reverb G2"),
    (r"varjo.*aero", "Varjo Aero"),
    (r"pimax.*crystal", "Pimax Crystal"),
];
const AUDIO: &[(&str, &str)] = &[
    (
        r"pico\s*streaming\s*microphone",
        "PICO Streaming Microphone",
    ),
    (r"pico\s*streaming\s*speaker", "PICO Streaming Speaker"),
    (r"steam streaming microphone", "Steam Streaming Microphone"),
    (r"steam streaming speakers", "Steam Streaming Speakers"),
    (r"oculus virtual audio", "Oculus Virtual Audio"),
    (r"virtual desktop audio", "Virtual Desktop Audio"),
    (r"alvr", "ALVR Audio"),
    (r"vive virtual audio", "VIVE Virtual Audio"),
    (
        r"vb-audio virtual cable|cable (?:input|output)",
        "VB-Audio Virtual Cable",
    ),
    (r"voicemeeter", "VoiceMeeter"),
];

const PHYSICAL_HINTS: &[&str] = &[
    "pico",
    "quest",
    "rift",
    "valve index",
    "knuckles",
    "vive",
    "tundra",
    "base station",
    "bigscreen",
    "reverb",
    "varjo",
    "pimax",
];
const EXCLUDED: &[&str] = &[
    "audio",
    "speaker",
    "microphone",
    "radio",
    "dongle",
    "driver",
    "bootloader",
];

pub const TRACKED: [&str; 4] = [
    "SteamVR headset",
    "SteamVR controller",
    "SteamVR tracker",
    "SteamVR base station",
];

fn label(
    value: &str,
    patterns: &[(&'static str, &'static str)],
    cache: &OnceLock<RegexSet>,
) -> Option<&'static str> {
    let catalog = cache.get_or_init(|| {
        regex::RegexSetBuilder::new(patterns.iter().map(|(pattern, _)| *pattern))
            .case_insensitive(true)
            .build()
            .expect("valid VR catalog")
    });
    catalog
        .matches(value)
        .into_iter()
        .next()
        .map(|index| patterns[index].1)
}

pub fn model(value: &str) -> Option<&'static str> {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    label(value, MODELS, &SET)
}

pub fn audio(value: &str) -> Option<&'static str> {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    label(value, AUDIO, &SET)
}

pub fn physical_name(value: &str) -> Option<&'static str> {
    let lower = value.to_lowercase();
    if !PHYSICAL_HINTS.iter().any(|hint| lower.contains(hint))
        || EXCLUDED.iter().any(|hint| lower.contains(hint))
    {
        return None;
    }
    model(value)
}

pub fn tracked_label(value: &str) -> bool {
    TRACKED.contains(&value) || MODELS.iter().any(|(_, label)| *label == value)
}

pub fn counted(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut counts = BTreeMap::new();
    for item in items {
        *counts.entry(item).or_insert(0u32) += 1;
    }
    counts
        .into_iter()
        .map(|(label, count)| match count {
            1 => label,
            _ => format!("{label} \u{00d7}{count}"),
        })
        .collect()
}

pub fn merge_connected(names: BTreeSet<String>, tracked: Vec<String>) -> Vec<String> {
    let other = names
        .into_iter()
        .filter(|name| !tracked.contains(name))
        .collect::<Vec<_>>();
    let mut out = counted(tracked);
    out.extend(other);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_does_not_echo_private_names_and_preserves_match_priority() {
        assert_eq!(
            model("Pico_4-enterprise serial-private"),
            Some("PICO 4 Enterprise/Pro")
        );
        assert_eq!(
            physical_name("Meta Quest 3S secret serial"),
            Some("Meta Quest 3S")
        );
        assert_eq!(physical_name("Pico 4 Streaming Microphone"), None);
        assert_eq!(physical_name("USB generic model serial-private"), None);
        assert_eq!(
            model("HTC Vive Tracker sensor 3.0"),
            Some("VIVE Tracker 3.0")
        );
        assert_eq!(
            audio("CABLE Output (VB-Audio Virtual Cable)"),
            Some("VB-Audio Virtual Cable")
        );
        assert!(!tracked_label("SteamVR headset private-serial"));
    }

    #[test]
    fn connected_prefers_counted_tracked_devices_before_unmatched_pnp() {
        let names = BTreeSet::from(["VIVE Tracker".into(), "Meta Quest 3".into()]);
        assert_eq!(
            merge_connected(names, vec!["VIVE Tracker".into(), "VIVE Tracker".into()]),
            ["VIVE Tracker \u{00d7}2", "Meta Quest 3"]
        );
    }
}
