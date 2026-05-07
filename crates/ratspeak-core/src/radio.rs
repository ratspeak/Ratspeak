use serde::Serialize;

pub const DEFAULT_RNODE_REGION_KEY: &str = "americas";
pub const DEFAULT_RNODE_PRESET_KEY: &str = "medium_fast";
pub const RNODE_FREQUENCY_MIN_HZ: u64 = 137_000_000;
pub const RNODE_FREQUENCY_MAX_HZ: u64 = 3_000_000_000;
pub const RNODE_BANDWIDTH_MIN_HZ: u64 = 7_800;
pub const RNODE_BANDWIDTH_MAX_HZ: u64 = 1_625_000;
pub const RNODE_SPREADING_FACTOR_MIN: u8 = 5;
pub const RNODE_SPREADING_FACTOR_MAX: u8 = 12;
pub const RNODE_CODING_RATE_MIN: u8 = 5;
pub const RNODE_CODING_RATE_MAX: u8 = 8;
pub const RNODE_TX_POWER_MIN_DBM: i8 = 0;
pub const RNODE_TX_POWER_MAX_DBM: i8 = 37;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RnodePreset {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub spreading_factor: u8,
    pub bandwidth: u64,
    pub coding_rate: u8,
    pub tx_power: i8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RnodeRegion {
    pub key: &'static str,
    pub label: &'static str,
    pub min: u64,
    pub max: u64,
    pub frequency: u64,
    pub airtime_limit_long: Option<f32>,
    pub airtime_limit_short: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RnodeParams {
    pub frequency: u64,
    pub bandwidth: u64,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power: i8,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct RnodeCatalog {
    pub default_region: &'static str,
    pub default_preset: &'static str,
    pub frequency_min: u64,
    pub frequency_max: u64,
    pub bandwidth_min: u64,
    pub bandwidth_max: u64,
    pub spreading_factor_min: u8,
    pub spreading_factor_max: u8,
    pub coding_rate_min: u8,
    pub coding_rate_max: u8,
    pub tx_power_min: i8,
    pub tx_power_max: i8,
    pub regions: &'static [RnodeRegion],
    pub presets: &'static [RnodePreset],
}

pub const RNODE_REGIONS: &[RnodeRegion] = &[
    RnodeRegion {
        key: "americas",
        label: "Americas (915 MHz)",
        min: 902_000_000,
        max: 928_000_000,
        frequency: 915_000_000,
        airtime_limit_long: None,
        airtime_limit_short: None,
    },
    RnodeRegion {
        key: "europe",
        label: "Europe (868 MHz)",
        min: 863_000_000,
        max: 870_000_000,
        frequency: 868_000_000,
        airtime_limit_long: Some(1.5),
        airtime_limit_short: Some(33.0),
    },
    RnodeRegion {
        key: "uhf_433",
        label: "433 MHz / UHF",
        min: 410_000_000,
        max: 525_000_000,
        frequency: 433_000_000,
        airtime_limit_long: None,
        airtime_limit_short: None,
    },
    RnodeRegion {
        key: "australia",
        label: "Australia (915 MHz)",
        min: 915_000_000,
        max: 928_000_000,
        frequency: 915_000_000,
        airtime_limit_long: None,
        airtime_limit_short: None,
    },
    RnodeRegion {
        key: "asia",
        label: "Asia (923 MHz)",
        min: 920_000_000,
        max: 925_000_000,
        frequency: 923_000_000,
        airtime_limit_long: None,
        airtime_limit_short: None,
    },
];

pub const RNODE_PRESETS: &[RnodePreset] = &[
    RnodePreset {
        key: "medium_fast",
        label: "Medium Fast",
        description: "Balanced field default. SF9, 250 kHz, CR 5.",
        spreading_factor: 9,
        bandwidth: 250_000,
        coding_rate: 5,
        tx_power: 17,
    },
    RnodePreset {
        key: "short_turbo",
        label: "Short Turbo",
        description: "Shortest range, highest data rate. SF7, 500 kHz, CR 5.",
        spreading_factor: 7,
        bandwidth: 500_000,
        coding_rate: 5,
        tx_power: 14,
    },
    RnodePreset {
        key: "short_fast",
        label: "Short Fast",
        description: "Short range, fast data rate. SF7, 250 kHz, CR 5.",
        spreading_factor: 7,
        bandwidth: 250_000,
        coding_rate: 5,
        tx_power: 14,
    },
    RnodePreset {
        key: "short_slow",
        label: "Short Slow",
        description: "Short range with more link margin. SF8, 250 kHz, CR 5.",
        spreading_factor: 8,
        bandwidth: 250_000,
        coding_rate: 5,
        tx_power: 14,
    },
    RnodePreset {
        key: "medium_slow",
        label: "Medium Slow",
        description: "More robust medium-range mode. SF10, 250 kHz, CR 5.",
        spreading_factor: 10,
        bandwidth: 250_000,
        coding_rate: 5,
        tx_power: 17,
    },
    RnodePreset {
        key: "long_turbo",
        label: "Long Turbo",
        description: "Long range with wide bandwidth. SF11, 500 kHz, CR 8.",
        spreading_factor: 11,
        bandwidth: 500_000,
        coding_rate: 8,
        tx_power: 22,
    },
    RnodePreset {
        key: "long_fast",
        label: "Long Fast",
        description: "Long range with moderate data rate. SF11, 250 kHz, CR 5.",
        spreading_factor: 11,
        bandwidth: 250_000,
        coding_rate: 5,
        tx_power: 22,
    },
    RnodePreset {
        key: "long_moderate",
        label: "Long Moderate",
        description: "Longer range, lower data rate. SF11, 125 kHz, CR 8.",
        spreading_factor: 11,
        bandwidth: 125_000,
        coding_rate: 8,
        tx_power: 22,
    },
];

pub fn rnode_catalog() -> RnodeCatalog {
    RnodeCatalog {
        default_region: DEFAULT_RNODE_REGION_KEY,
        default_preset: DEFAULT_RNODE_PRESET_KEY,
        frequency_min: RNODE_FREQUENCY_MIN_HZ,
        frequency_max: RNODE_FREQUENCY_MAX_HZ,
        bandwidth_min: RNODE_BANDWIDTH_MIN_HZ,
        bandwidth_max: RNODE_BANDWIDTH_MAX_HZ,
        spreading_factor_min: RNODE_SPREADING_FACTOR_MIN,
        spreading_factor_max: RNODE_SPREADING_FACTOR_MAX,
        coding_rate_min: RNODE_CODING_RATE_MIN,
        coding_rate_max: RNODE_CODING_RATE_MAX,
        tx_power_min: RNODE_TX_POWER_MIN_DBM,
        tx_power_max: RNODE_TX_POWER_MAX_DBM,
        regions: RNODE_REGIONS,
        presets: RNODE_PRESETS,
    }
}

pub fn rnode_region(key: &str) -> Option<&'static RnodeRegion> {
    RNODE_REGIONS.iter().find(|region| region.key == key)
}

pub fn rnode_preset(key: &str) -> Option<&'static RnodePreset> {
    RNODE_PRESETS.iter().find(|preset| preset.key == key)
}

pub fn resolve_rnode_params(region_key: &str, preset_key: &str) -> Option<RnodeParams> {
    let region = rnode_region(region_key)?;
    let preset = rnode_preset(preset_key)?;
    Some(RnodeParams {
        frequency: region.frequency,
        bandwidth: preset.bandwidth,
        spreading_factor: preset.spreading_factor,
        coding_rate: preset.coding_rate,
        tx_power: preset.tx_power,
    })
}

pub fn default_rnode_params() -> RnodeParams {
    resolve_rnode_params(DEFAULT_RNODE_REGION_KEY, DEFAULT_RNODE_PRESET_KEY)
        .expect("default RNode region and preset must resolve")
}

pub fn infer_rnode_region(frequency: u64) -> Option<&'static str> {
    if let Some(region) = RNODE_REGIONS
        .iter()
        .find(|region| region.frequency == frequency)
    {
        return Some(region.key);
    }

    RNODE_REGIONS
        .iter()
        .find(|region| region.min <= frequency && frequency <= region.max)
        .map(|region| region.key)
}

pub fn infer_rnode_preset(
    bandwidth: u64,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power: i8,
) -> Option<&'static str> {
    RNODE_PRESETS
        .iter()
        .find(|preset| {
            preset.bandwidth == bandwidth
                && preset.spreading_factor == spreading_factor
                && preset.coding_rate == coding_rate
                && preset.tx_power == tx_power
        })
        .map(|preset| preset.key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn rnode_catalog_keys_are_unique_and_defaults_exist() {
        let region_keys = RNODE_REGIONS
            .iter()
            .map(|region| region.key)
            .collect::<HashSet<_>>();
        let preset_keys = RNODE_PRESETS
            .iter()
            .map(|preset| preset.key)
            .collect::<HashSet<_>>();

        assert_eq!(region_keys.len(), RNODE_REGIONS.len());
        assert_eq!(preset_keys.len(), RNODE_PRESETS.len());
        assert!(region_keys.contains(DEFAULT_RNODE_REGION_KEY));
        assert!(preset_keys.contains(DEFAULT_RNODE_PRESET_KEY));
    }

    #[test]
    fn default_rnode_params_match_launch_radio_default() {
        assert_eq!(
            default_rnode_params(),
            RnodeParams {
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
            }
        );
    }

    #[test]
    fn all_regions_and_presets_are_inside_generic_radio_bounds() {
        for region in RNODE_REGIONS {
            assert!(region.min <= region.frequency);
            assert!(region.frequency <= region.max);
            assert!(region.min >= RNODE_FREQUENCY_MIN_HZ);
            assert!(region.max <= RNODE_FREQUENCY_MAX_HZ);
        }

        for preset in RNODE_PRESETS {
            assert!(
                (RNODE_SPREADING_FACTOR_MIN..=RNODE_SPREADING_FACTOR_MAX)
                    .contains(&preset.spreading_factor)
            );
            assert!((RNODE_BANDWIDTH_MIN_HZ..=RNODE_BANDWIDTH_MAX_HZ).contains(&preset.bandwidth));
            assert!((RNODE_CODING_RATE_MIN..=RNODE_CODING_RATE_MAX).contains(&preset.coding_rate));
            assert!((RNODE_TX_POWER_MIN_DBM..=RNODE_TX_POWER_MAX_DBM).contains(&preset.tx_power));
        }
    }

    #[test]
    fn exact_numeric_configs_can_infer_catalog_keys() {
        assert_eq!(infer_rnode_region(915_000_000), Some("americas"));
        assert_eq!(infer_rnode_region(915_250_000), Some("americas"));
        assert_eq!(infer_rnode_region(433_000_000), Some("uhf_433"));
        assert_eq!(infer_rnode_preset(250_000, 9, 5, 17), Some("medium_fast"));
        assert_eq!(
            infer_rnode_preset(125_000, 11, 8, 22),
            Some("long_moderate")
        );
    }
}
