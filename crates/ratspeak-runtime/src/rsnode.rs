//! rsNode Repeaters managed by message: their `FIELD_RESULTS` replies, and firmware updates over
//! the node's `rsnode.manage` Link ([`update`]).

use rmpv::Value as Msgpack;
use serde_json::{Map, Value, json};

pub mod update;

/// `FIELD_RESULTS` key 0x00: a schema version marks an rsNode reply.
const RESULT_SCHEMA: u64 = 0x00;
/// Bound on the encoded field; a reply fits one LoRa packet.
const RESULTS_MAX_BYTES: usize = 1024;

/// Decode `FIELD_RESULTS` (a msgpack map with integer keys) into named JSON fields. Unknown
/// keys and ill-typed values are dropped; `None` when the field is not such a map.
pub fn decode_results(mut encoded: &[u8]) -> Option<Value> {
    if encoded.len() > RESULTS_MAX_BYTES {
        return None;
    }
    let Msgpack::Map(entries) = rmpv::decode::read_value(&mut encoded).ok()? else {
        return None;
    };
    if !encoded.is_empty() {
        return None;
    }
    let mut out = Map::new();
    for (key, value) in &entries {
        let Some(key) = key.as_u64() else {
            continue;
        };
        let decoded = match key {
            RESULT_SCHEMA => value.as_u64().map(|v| ("schema", json!(v))),
            0x01 => value.as_str().map(|v| ("command", json!(v))),
            0x02 => value.as_str().map(|v| ("release", json!(v))),
            0x03 => value.as_bool().map(|v| ("running", json!(v))),
            0x04 => u32_of(value).map(|v| ("uptime_s", json!(v))),
            0x05 => {
                u8_of(value).map(|v| ("region", if v == 0xFF { Value::Null } else { json!(v) }))
            }
            0x06 => radio(value).map(|v| ("radio", v)),
            0x07 => u8_of(value).and_then(|v| {
                ["default", "custom", "off"]
                    .get(usize::from(v))
                    .map(|name| ("limits", json!(name)))
            }),
            0x08 => u32_of(value).map(|v| ("announce_interval_s", json!(v))),
            0x09 => u8_of(value).and_then(|v| {
                ["off", "connecting", "up"]
                    .get(usize::from(v))
                    .map(|name| ("uplink", json!(name)))
            }),
            0x0A => u8_of(value).map(|v| ("peers", json!(v))),
            0x0B => counters(value).map(|v| ("packets", v)),
            0x0C => u8_of(value).map(|v| ("airtime_percent", json!(v.min(100)))),
            0x0D => value
                .as_slice()
                .filter(|bytes| bytes.len() == 3)
                .map(|bytes| ("board", json!(hex::encode(bytes)))),
            _ => None,
        };
        if let Some((name, value)) = decoded {
            out.insert(name.into(), value);
        }
    }
    Some(Value::Object(out))
}

/// Results that mark their sender as an rsNode: they carry a schema version.
pub fn marks_rsnode(results: &Value) -> bool {
    results.get("schema").is_some_and(Value::is_u64)
}

fn u32_of(value: &Msgpack) -> Option<u32> {
    value.as_u64().and_then(|v| u32::try_from(v).ok())
}

fn u8_of(value: &Msgpack) -> Option<u8> {
    value.as_u64().and_then(|v| u8::try_from(v).ok())
}

fn radio(value: &Msgpack) -> Option<Value> {
    let items = value.as_array().filter(|items| items.len() == 5)?;
    Some(json!({
        "frequency_hz": u32_of(&items[0])?,
        "bandwidth_hz": u32_of(&items[1])?,
        "sf": u8_of(&items[2])?,
        "cr": u8_of(&items[3])?,
        "power_dbm": items[4].as_i64().filter(|v| (-128..=127).contains(v))?,
    }))
}

fn counters(value: &Msgpack) -> Option<Value> {
    let items = value.as_array().filter(|items| items.len() == 2)?;
    Some(json!({ "rx": u32_of(&items[0])?, "tx": u32_of(&items[1])? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(entries: Vec<(Msgpack, Msgpack)>) -> Vec<u8> {
        let mut out = Vec::new();
        rmpv::encode::write_value(&mut out, &Msgpack::Map(entries)).unwrap();
        out
    }

    fn int(v: i64) -> Msgpack {
        Msgpack::from(v)
    }

    #[test]
    fn status_results_decode_to_named_fields() {
        let encoded = encode(vec![
            (int(0), int(1)),
            (int(1), Msgpack::from("status")),
            (int(2), Msgpack::from("1.0.1")),
            (int(3), Msgpack::Boolean(true)),
            (int(4), int(3_600)),
            (int(5), int(1)),
            (
                int(6),
                Msgpack::Array(vec![
                    int(914_875_000),
                    int(250_000),
                    int(9),
                    int(5),
                    int(-3),
                ]),
            ),
            (int(7), int(2)),
            (int(8), int(1_800)),
            (int(9), int(2)),
            (int(10), int(1)),
            (int(13), Msgpack::Binary(vec![0x80, 0x81, 0x3A])),
            (int(0x7F), Msgpack::from("future")),
        ]);
        let results = decode_results(&encoded).unwrap();
        assert_eq!(
            results,
            json!({
                "schema": 1,
                "command": "status",
                "release": "1.0.1",
                "running": true,
                "uptime_s": 3600,
                "region": 1,
                "radio": {"frequency_hz": 914875000, "bandwidth_hz": 250000, "sf": 9, "cr": 5, "power_dbm": -3},
                "limits": "off",
                "announce_interval_s": 1800,
                "uplink": "up",
                "peers": 1,
                "board": "80813a",
            })
        );
        assert!(marks_rsnode(&results));
    }

    #[test]
    fn stats_results_and_bad_shapes() {
        let encoded = encode(vec![
            (int(0), int(1)),
            (int(1), Msgpack::from("stats")),
            (int(5), int(0xFF)),
            (int(11), Msgpack::Array(vec![int(10), int(4)])),
            (int(12), int(250)),
            // Ill-typed values and text keys are dropped, not fatal.
            (int(4), Msgpack::from("soon")),
            (Msgpack::from("x"), int(1)),
        ]);
        let results = decode_results(&encoded).unwrap();
        assert_eq!(results["packets"], json!({"rx": 10, "tx": 4}));
        assert_eq!(results["airtime_percent"], 100);
        assert!(results["region"].is_null());
        assert!(results.get("uptime_s").is_none());

        // No schema key: decoded, but not an rsNode marker.
        let plain = decode_results(&encode(vec![(int(1), Msgpack::from("status"))])).unwrap();
        assert!(!marks_rsnode(&plain));
        assert!(decode_results(&[0x92, 0x01, 0x02]).is_none(), "not a map");
        let mut trailing = encode(vec![(int(0), int(1))]);
        trailing.push(0xC0);
        assert!(decode_results(&trailing).is_none());
    }
}
