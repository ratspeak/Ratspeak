//! Reticulum's announce ingress control as Activity tells it: one start row
//! and one end row per pacing spell on a connection.

use std::collections::BTreeMap;
use std::time::Duration;

use rns_link::clock::ContinuousInstant;
use rns_transport::messages::InterfaceStatRpcEntry;

/// Pacing that starts again this soon after it ended continues the same spell.
pub(crate) const CALM: Duration = Duration::from_secs(10 * 60);

/// A connection as the Network screen lists it: by its key and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PacedInterface {
    pub(crate) key: u64,
    pub(crate) name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AnnouncePacingChange {
    Started(PacedInterface),
    /// The row comes after the calm window; `ago` is how long before it pacing stopped.
    Ended {
        interface: PacedInterface,
        ago: Duration,
    },
}

impl AnnouncePacingChange {
    pub(crate) fn key(&self) -> u64 {
        match self {
            Self::Started(interface) | Self::Ended { interface, .. } => interface.key,
        }
    }
}

struct Spell {
    name: String,
    /// When pacing last stopped; `None` while it is on.
    ended_at: Option<ContinuousInstant>,
}

#[derive(Default)]
pub(crate) struct AnnouncePacing {
    spells: BTreeMap<u64, Spell>,
}

impl AnnouncePacing {
    /// Folds one interface-stats poll in and returns the rows it starts or ends.
    pub(crate) fn observe(
        &mut self,
        stats: &[InterfaceStatRpcEntry],
        now: ContinuousInstant,
    ) -> Vec<AnnouncePacingChange> {
        let connections = connections(stats);
        let mut changes = Vec::new();
        // Calm spells end first, so pacing after the calm window starts anew.
        let ended: Vec<u64> = self
            .spells
            .iter()
            .filter(|(key, spell)| {
                spell
                    .ended_at
                    .is_some_and(|at| now.saturating_duration_since(at) >= CALM)
                    || connections
                        .get(*key)
                        .is_some_and(|(name, _)| *name != spell.name)
            })
            .map(|(key, _)| *key)
            .collect();
        for key in ended {
            changes.extend(self.end(key, now));
        }
        for (key, (name, pacing)) in &connections {
            match self.spells.get_mut(key) {
                Some(spell) if *pacing => spell.ended_at = None,
                Some(spell) => {
                    spell.ended_at.get_or_insert(now);
                }
                None if *pacing => {
                    self.spells.insert(
                        *key,
                        Spell {
                            name: name.clone(),
                            ended_at: None,
                        },
                    );
                    changes.push(AnnouncePacingChange::Started(PacedInterface {
                        key: *key,
                        name: name.clone(),
                    }));
                }
                None => {}
            }
        }
        // A connection that went away is no longer pacing.
        for (key, spell) in &mut self.spells {
            if !connections.contains_key(key) {
                spell.ended_at.get_or_insert(now);
            }
        }
        changes
    }

    /// Ends every open spell, when polling stops.
    pub(crate) fn close_all(&mut self, now: ContinuousInstant) -> Vec<AnnouncePacingChange> {
        let keys: Vec<u64> = self.spells.keys().copied().collect();
        keys.into_iter()
            .filter_map(|key| self.end(key, now))
            .collect()
    }

    fn end(&mut self, key: u64, now: ContinuousInstant) -> Option<AnnouncePacingChange> {
        let spell = self.spells.remove(&key)?;
        Some(AnnouncePacingChange::Ended {
            interface: PacedInterface {
                key,
                name: spell.name,
            },
            ago: spell
                .ended_at
                .map_or(Duration::ZERO, |at| now.saturating_duration_since(at)),
        })
    }
}

/// The names of the connections pacing new announces now.
pub fn pacing_connections(stats: &[InterfaceStatRpcEntry]) -> Vec<String> {
    connections(stats)
        .into_values()
        .filter_map(|(name, pacing)| pacing.then_some(name))
        .collect()
}

/// Each connection's name and whether it is pacing. A listener's spawned
/// clients count as the listener, as the Network screen shows them.
fn connections(stats: &[InterfaceStatRpcEntry]) -> BTreeMap<u64, (String, bool)> {
    let mut connections = BTreeMap::new();
    for row in stats {
        let parent = row
            .parent_id
            .and_then(|id| stats.iter().find(|p| p.id == id))
            .or_else(|| {
                row.parent_interface_name
                    .as_deref()
                    .and_then(|name| stats.iter().find(|p| p.name == name))
            });
        let owner = parent.unwrap_or(row);
        connections
            .entry(owner.id)
            .or_insert_with(|| (owner.name.clone(), false))
            .1 |= row.burst_active;
    }
    connections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u64, name: &str, pacing: bool) -> InterfaceStatRpcEntry {
        let mut row = InterfaceStatRpcEntry::default();
        row.id = id;
        row.name = name.to_string();
        row.burst_active = pacing;
        row
    }

    fn interface(key: u64, name: &str) -> PacedInterface {
        PacedInterface {
            key,
            name: name.to_string(),
        }
    }

    fn started(key: u64, name: &str) -> AnnouncePacingChange {
        AnnouncePacingChange::Started(interface(key, name))
    }

    /// Ended, with pacing stopped `ago` minutes before the row.
    fn ended(key: u64, name: &str, ago: u64) -> AnnouncePacingChange {
        AnnouncePacingChange::Ended {
            interface: interface(key, name),
            ago: Duration::from_secs(ago * 60),
        }
    }

    fn minutes(at: ContinuousInstant, minutes: u64) -> ContinuousInstant {
        at + Duration::from_secs(minutes * 60)
    }

    #[test]
    fn one_connection_starts_and_ends_once() {
        let t0 = ContinuousInstant::now();
        let mut pacing = AnnouncePacing::default();
        assert!(pacing.observe(&[row(1, "Hub", false)], t0).is_empty());
        assert_eq!(
            pacing.observe(&[row(1, "Hub", true)], minutes(t0, 1)),
            [started(1, "Hub")]
        );
        assert!(
            pacing
                .observe(&[row(1, "Hub", true)], minutes(t0, 2))
                .is_empty()
        );
        assert!(
            pacing
                .observe(&[row(1, "Hub", false)], minutes(t0, 3))
                .is_empty()
        );
        // The end row waits out the calm window.
        assert!(
            pacing
                .observe(&[row(1, "Hub", false)], minutes(t0, 12))
                .is_empty()
        );
        assert_eq!(
            pacing.observe(&[row(1, "Hub", false)], minutes(t0, 13)),
            [ended(1, "Hub", 10)]
        );
        assert!(
            pacing
                .observe(&[row(1, "Hub", false)], minutes(t0, 30))
                .is_empty()
        );
    }

    #[test]
    fn two_connections_pace_on_their_own() {
        let t0 = ContinuousInstant::now();
        let mut pacing = AnnouncePacing::default();
        assert_eq!(
            pacing.observe(&[row(1, "Hub", true), row(2, "Radio", true)], t0),
            [started(1, "Hub"), started(2, "Radio")]
        );
        let both = |hub, radio| [row(1, "Hub", hub), row(2, "Radio", radio)];
        assert!(
            pacing
                .observe(&both(false, true), minutes(t0, 1))
                .is_empty()
        );
        assert_eq!(
            pacing.observe(&both(false, false), minutes(t0, 11)),
            [ended(1, "Hub", 10)]
        );
        assert_eq!(
            pacing.observe(&both(false, false), minutes(t0, 21)),
            [ended(2, "Radio", 10)]
        );
    }

    #[test]
    fn pacing_again_within_ten_minutes_extends_the_spell() {
        let t0 = ContinuousInstant::now();
        let mut pacing = AnnouncePacing::default();
        assert_eq!(
            pacing.observe(&[row(1, "Hub", true)], t0),
            [started(1, "Hub")]
        );
        assert!(
            pacing
                .observe(&[row(1, "Hub", false)], minutes(t0, 1))
                .is_empty()
        );
        // Nine minutes after it ended: no new start row.
        assert!(
            pacing
                .observe(&[row(1, "Hub", true)], minutes(t0, 10))
                .is_empty()
        );
        assert!(
            pacing
                .observe(&[row(1, "Hub", false)], minutes(t0, 11))
                .is_empty()
        );
        assert!(
            pacing
                .observe(&[row(1, "Hub", false)], minutes(t0, 20))
                .is_empty()
        );
        assert_eq!(
            pacing.observe(&[row(1, "Hub", false)], minutes(t0, 21)),
            [ended(1, "Hub", 10)]
        );
    }

    #[test]
    fn pacing_again_after_ten_minutes_adds_a_new_pair() {
        let t0 = ContinuousInstant::now();
        let mut pacing = AnnouncePacing::default();
        pacing.observe(&[row(1, "Hub", true)], t0);
        pacing.observe(&[row(1, "Hub", false)], minutes(t0, 1));
        // No poll lands in the window, as when the app was asleep: the old
        // spell still ends before the new one starts, eleven minutes after it stopped.
        assert_eq!(
            pacing.observe(&[row(1, "Hub", true)], minutes(t0, 12)),
            [ended(1, "Hub", 11), started(1, "Hub")]
        );
        pacing.observe(&[row(1, "Hub", false)], minutes(t0, 13));
        assert_eq!(
            pacing.observe(&[row(1, "Hub", false)], minutes(t0, 23)),
            [ended(1, "Hub", 10)]
        );
    }

    #[test]
    fn spawned_clients_pace_as_their_listener() {
        let t0 = ContinuousInstant::now();
        let mut by_id = row(11, "Client on Hub", true);
        by_id.parent_id = Some(10);
        let mut by_name = row(12, "Client on Hub", true);
        by_name.parent_interface_name = Some("Hub".into());
        let stats = [row(10, "Hub", false), by_id, by_name];
        let mut pacing = AnnouncePacing::default();
        assert_eq!(pacing.observe(&stats, t0), [started(10, "Hub")]);
        assert_eq!(pacing_connections(&stats), ["Hub"]);
        assert_eq!(pacing.close_all(t0), [ended(10, "Hub", 0)]);
    }

    #[test]
    fn a_connection_that_goes_away_ends_after_the_calm_window() {
        let t0 = ContinuousInstant::now();
        let mut pacing = AnnouncePacing::default();
        pacing.observe(&[row(1, "Hub", true)], t0);
        assert!(pacing.observe(&[], minutes(t0, 1)).is_empty());
        assert_eq!(pacing.observe(&[], minutes(t0, 11)), [ended(1, "Hub", 10)]);
    }

    #[test]
    fn a_key_that_now_names_another_connection_ends_the_old_spell() {
        let t0 = ContinuousInstant::now();
        let mut pacing = AnnouncePacing::default();
        pacing.observe(&[row(1, "Hub", true)], t0);
        assert_eq!(
            pacing.observe(&[row(1, "Radio", true)], minutes(t0, 1)),
            [ended(1, "Hub", 0), started(1, "Radio")]
        );
    }

    #[test]
    fn stopping_ends_open_spells_at_once() {
        let t0 = ContinuousInstant::now();
        let mut pacing = AnnouncePacing::default();
        pacing.observe(&[row(1, "Hub", true), row(2, "Radio", true)], t0);
        pacing.observe(
            &[row(1, "Hub", true), row(2, "Radio", false)],
            minutes(t0, 1),
        );
        assert_eq!(
            pacing.close_all(minutes(t0, 4)),
            [ended(1, "Hub", 0), ended(2, "Radio", 3)]
        );
        assert!(pacing.close_all(minutes(t0, 5)).is_empty());
    }
}
