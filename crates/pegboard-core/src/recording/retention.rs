//! The retention rules (spec 4.7) as a pure plan: which recordings to delete, and which to
//! keep without their screen. `RecordingStore::prune` gathers the facts and carries it out.

use serde::Serialize;
use time::OffsetDateTime;

use crate::recording::config::RecordingConfig;

/// What the plan needs to know of one recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub id: String,
    pub started_at: OffsetDateTime,
    /// When it ended; `None` only while it runs.
    pub ended_at: Option<OffsetDateTime>,
    pub running: bool,
    pub keep: bool,
    /// Bytes under `screen/`.
    pub screen_bytes: u64,
    /// Every other byte of the recording (manifest, action log, line logs).
    pub other_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum PruneAction {
    /// The whole recording.
    Delete { id: String },
    /// Its `screen/` only; the action log and the line logs stay.
    DeleteScreen { id: String },
}

impl PruneAction {
    pub fn id(&self) -> &str {
        match self {
            PruneAction::Delete { id } | PruneAction::DeleteScreen { id } => id,
        }
    }
}

/// Pure. 1) not running, not kept, ended_at + max_age <= now => Delete.
/// 2) then, while the bytes of every recording that is not kept (running ones included) exceed
///    max_total_bytes: DeleteScreen of the not-running, not-kept recording with the oldest
///    ended_at that still has screen bytes. Action logs are never deleted for size.
pub fn plan(recs: &[Facts], now: OffsetDateTime, cfg: &RecordingConfig) -> Vec<PruneAction> {
    let max_age = cfg.max_age();
    let mut actions = Vec::new();
    let mut left: Vec<&Facts> = Vec::new();
    for r in recs {
        let expired = !r.running && !r.keep && r.ended_at.is_some_and(|end| end + max_age <= now);
        if expired {
            actions.push(PruneAction::Delete { id: r.id.clone() });
        } else {
            left.push(r);
        }
    }
    let mut counted: u64 = left
        .iter()
        .filter(|r| !r.keep)
        .map(|r| r.screen_bytes.saturating_add(r.other_bytes))
        .fold(0, u64::saturating_add);
    let mut screens: Vec<&Facts> = left
        .into_iter()
        .filter(|r| !r.running && !r.keep && r.screen_bytes > 0)
        .collect();
    // Oldest end first; a recording without an end (not running, so cut off) counts from
    // its start. The id breaks ties so the plan does not depend on the input order.
    screens.sort_by(|a, b| {
        (a.ended_at.unwrap_or(a.started_at), &a.id)
            .cmp(&(b.ended_at.unwrap_or(b.started_at), &b.id))
    });
    for r in screens {
        if counted <= cfg.max_total_bytes() {
            break;
        }
        actions.push(PruneAction::DeleteScreen { id: r.id.clone() });
        counted = counted.saturating_sub(r.screen_bytes);
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    const GB: u64 = 1_000_000_000;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(2_000_000_000).unwrap()
    }

    fn finished(id: &str, ended_ago: Duration, screen: u64) -> Facts {
        let ended = now() - ended_ago;
        Facts {
            id: id.into(),
            started_at: ended - Duration::minutes(10),
            ended_at: Some(ended),
            running: false,
            keep: false,
            screen_bytes: screen,
            other_bytes: 1000,
        }
    }

    fn cfg() -> RecordingConfig {
        RecordingConfig::default()
    }

    fn delete(id: &str) -> PruneAction {
        PruneAction::Delete { id: id.into() }
    }

    fn screen(id: &str) -> PruneAction {
        PruneAction::DeleteScreen { id: id.into() }
    }

    #[test]
    fn age_rule() {
        let fourteen = Duration::days(14);
        let recs = [
            finished("r_aaaaaaaa", fourteen + Duration::seconds(1), 10),
            finished("r_bbbbbbbb", fourteen - Duration::seconds(1), 10),
            finished("r_cccccccc", fourteen, 10),
            // Before 2b: no screen at all.
            finished("r_dddddddd", fourteen + Duration::seconds(1), 0),
            finished("r_eeeeeeee", fourteen - Duration::seconds(1), 0),
        ];
        assert_eq!(
            plan(&recs, now(), &cfg()),
            vec![
                delete("r_aaaaaaaa"),
                delete("r_cccccccc"),
                delete("r_dddddddd")
            ]
        );
        // The setting counts.
        let short = RecordingConfig {
            max_age_days: 1,
            ..cfg()
        };
        assert_eq!(plan(&recs, now(), &short).len(), 5);
    }

    #[test]
    fn prune_never_deletes_a_running_or_kept_recording() {
        let running = Facts {
            id: "r_runnnnnn".into(),
            started_at: now() - Duration::days(30),
            ended_at: None,
            running: true,
            keep: false,
            screen_bytes: 10 * GB,
            other_bytes: 0,
        };
        let kept = Facts {
            keep: true,
            ..finished("r_keptkept", Duration::days(30), 10 * GB)
        };
        let tight = RecordingConfig {
            max_age_days: 1,
            max_total_gb: 0.001,
            ..cfg()
        };
        for c in [cfg(), tight] {
            assert_eq!(plan(&[running.clone(), kept.clone()], now(), &c), vec![]);
        }
        // A running one that was given an end (it has not ended: it still runs) is not old.
        let odd = Facts {
            ended_at: Some(now() - Duration::days(30)),
            ..running
        };
        assert_eq!(plan(&[odd], now(), &cfg()), vec![]);
    }

    #[test]
    fn size_rule_deletes_screens_oldest_first_and_stops_under_the_limit() {
        let recs = [
            finished("r_cccccccc", Duration::hours(2), 2 * GB),
            finished("r_aaaaaaaa", Duration::hours(4), 2 * GB),
            finished("r_dddddddd", Duration::hours(1), 2 * GB),
            finished("r_bbbbbbbb", Duration::hours(3), 2 * GB),
        ];
        assert_eq!(
            plan(&recs, now(), &cfg()),
            vec![screen("r_aaaaaaaa"), screen("r_bbbbbbbb")]
        );
    }

    #[test]
    fn kept_bytes_do_not_count_and_running_bytes_do() {
        let kept = Facts {
            keep: true,
            ..finished("r_keptkept", Duration::hours(5), 100 * GB)
        };
        let old = finished("r_oldoldol", Duration::hours(4), 2 * GB);
        // Under the limit with the kept one left out.
        assert_eq!(plan(&[kept.clone(), old.clone()], now(), &cfg()), vec![]);
        // A running recording counts, and is not the one that loses its screen.
        let running = Facts {
            id: "r_runnnnnn".into(),
            started_at: now() - Duration::hours(6),
            ended_at: None,
            running: true,
            keep: false,
            screen_bytes: 4 * GB,
            other_bytes: 0,
        };
        assert_eq!(
            plan(&[kept, old, running], now(), &cfg()),
            vec![screen("r_oldoldol")]
        );
    }

    #[test]
    fn action_logs_are_never_deleted_for_size() {
        let mut recs: Vec<Facts> = (0..4)
            .map(|i| Facts {
                other_bytes: 3 * GB,
                ..finished(&format!("r_aaaaaaa{i}"), Duration::hours(4 - i), GB)
            })
            .collect();
        recs.push(Facts {
            other_bytes: 3 * GB,
            ..finished("r_noscreen", Duration::hours(9), 0)
        });
        let actions = plan(&recs, now(), &cfg());
        assert_eq!(actions.len(), 4, "{actions:?}");
        assert!(
            actions
                .iter()
                .all(|a| matches!(a, PruneAction::DeleteScreen { .. })),
            "{actions:?}"
        );
    }

    #[test]
    fn a_pruned_screen_takes_only_its_own_bytes_off() {
        // 7 GB: A's screen goes (its 3 GB of actions stay, 6 GB left), then B's (4 GB).
        let recs = [
            Facts {
                other_bytes: 3 * GB,
                ..finished("r_aaaaaaaa", Duration::hours(3), GB)
            },
            Facts {
                other_bytes: 0,
                ..finished("r_bbbbbbbb", Duration::hours(2), 2 * GB)
            },
            Facts {
                other_bytes: 0,
                ..finished("r_cccccccc", Duration::hours(1), GB)
            },
        ];
        assert_eq!(
            plan(&recs, now(), &cfg()),
            vec![screen("r_aaaaaaaa"), screen("r_bbbbbbbb")]
        );
    }

    #[test]
    fn the_oldest_end_goes_first_not_the_oldest_start() {
        // A started first but ran long; B started later and ended first.
        let a = Facts {
            started_at: now() - Duration::hours(10),
            ..finished("r_aaaaaaaa", Duration::hours(1), 3 * GB)
        };
        let b = Facts {
            started_at: now() - Duration::hours(5),
            ..finished("r_bbbbbbbb", Duration::hours(4), 3 * GB)
        };
        assert_eq!(plan(&[a, b], now(), &cfg()), vec![screen("r_bbbbbbbb")]);
    }

    #[test]
    fn exactly_the_limit_is_within_it() {
        let at_limit = |id: &str, ago: Duration, screen_bytes: u64| Facts {
            other_bytes: 0,
            ..finished(id, ago, screen_bytes)
        };
        let recs = [
            at_limit("r_aaaaaaaa", Duration::hours(2), 2 * GB),
            at_limit("r_bbbbbbbb", Duration::hours(1), 3 * GB),
        ];
        assert_eq!(plan(&recs, now(), &cfg()), vec![]);
        // One byte over: the oldest screen goes, which brings it to the limit exactly.
        let recs = [
            at_limit("r_aaaaaaaa", Duration::hours(3), 1),
            at_limit("r_bbbbbbbb", Duration::hours(2), 2 * GB),
            at_limit("r_cccccccc", Duration::hours(1), 3 * GB),
        ];
        assert_eq!(plan(&recs, now(), &cfg()), vec![screen("r_aaaaaaaa")]);
    }

    #[test]
    fn deleted_by_age_does_not_count_for_size() {
        let recs = [
            finished("r_aaaaaaaa", Duration::days(20), 6 * GB),
            finished("r_bbbbbbbb", Duration::hours(1), 2 * GB),
        ];
        assert_eq!(plan(&recs, now(), &cfg()), vec![delete("r_aaaaaaaa")]);
    }

    #[test]
    fn actions_are_tagged_for_the_wire() {
        assert_eq!(
            serde_json::to_value(screen("r_aaaaaaaa")).unwrap(),
            serde_json::json!({"action": "deleteScreen", "id": "r_aaaaaaaa"})
        );
        assert_eq!(
            serde_json::to_value(delete("r_aaaaaaaa")).unwrap(),
            serde_json::json!({"action": "delete", "id": "r_aaaaaaaa"})
        );
    }
}
