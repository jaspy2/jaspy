// Fleet issue derivation.
//
// nexus detects many fault conditions but scatters them across per-device and
// per-interface views. This module rolls them into a single flat list of
// `DerivedIssue`s built from already-assembled API structs, so the mapping
// (condition -> issue) stays pure and unit-testable while the plumbing (locks,
// DB, topology) lives in the route handler that feeds it (routes/api/v1.rs).
//
// Issues have no stored identity: the key is the deterministic composite
// "<fqdn>|<kind>|<subject>", stable across re-derivation and restarts.
// `IssueTracker` stamps a first_seen/last_seen per key so the UI gets real
// onset times and an acknowledgement can bind to one occurrence — a
// cleared-then-recurring condition gets a fresh first_seen and re-alerts.
use std::collections::{HashMap, HashSet};
use crate::models::json;

pub const SEV_WARN: &str = "warn";
pub const SEV_BAD: &str = "bad";

// The hostname is the first DNS label; empty fqdn (network-level issues) stays
// empty.
fn hostname_of(fqdn: &str) -> String {
    if fqdn.is_empty() {
        String::new()
    } else {
        fqdn.split('.').next().unwrap_or(fqdn).to_string()
    }
}

fn pair(k: &str, v: impl Into<String>) -> (String, String) {
    (k.to_string(), v.into())
}

#[derive(Clone, Debug, PartialEq)]
pub struct DerivedIssue {
    pub fqdn: String,
    pub hostname: String,
    pub kind: String,
    pub subject: String,
    pub severity: String,
    pub title: String,
    pub description: String,
    pub subject_label: Option<String>,
    pub detail: Vec<(String, String)>,
}

impl DerivedIssue {
    pub fn issue_key(&self) -> String {
        format!("{}|{}|{}", self.fqdn, self.kind, self.subject)
    }
}

// --- Device ---------------------------------------------------------------

pub fn device_down_issue(fqdn: &str, up: Option<bool>, seconds_since_last_poll: Option<u64>) -> Option<DerivedIssue> {
    if up != Some(false) {
        return None;
    }
    let host = hostname_of(fqdn);
    let mut detail = vec![pair("state", "down")];
    if let Some(secs) = seconds_since_last_poll {
        detail.push(pair("seconds since last poll", secs.to_string()));
    }
    Some(DerivedIssue {
        fqdn: fqdn.to_string(),
        hostname: host.clone(),
        kind: "device-down".to_string(),
        subject: String::new(),
        severity: SEV_BAD.to_string(),
        title: "Device down".to_string(),
        description: format!("{} is not responding", host),
        subject_label: None,
        detail,
    })
}

// --- PoE budget -----------------------------------------------------------

// Utilization percent we warn at when the switch reports no configured
// pethMainPseUsageThreshold (0/unset), and the ceiling above which a PoE budget
// is critical regardless of the configured threshold.
const POE_DEFAULT_WARN_PCT: i64 = 85;
const POE_CRITICAL_PCT: i64 = 95;

// One issue per PSE group that is either not operational (bad) or running near
// its power budget (warn, escalating to bad past POE_CRITICAL_PCT). A device
// with several PSE groups (stacked/modular) yields one issue per group, each
// independently acknowledgeable via its group subject.
pub fn poe_budget_issues(fqdn: &str, budgets: &[crate::collectors::poe::PoeBudget]) -> Vec<DerivedIssue> {
    let host = hostname_of(fqdn);
    let mut out = Vec::new();
    for budget in budgets {
        let subject = budget.group.to_string();
        let subject_label = Some(format!("PSE {}", budget.group));

        // A PSE that isn't on can't source power; its budget figures are
        // meaningless, so report the outage instead of a utilization warning.
        if !budget.oper_on {
            out.push(DerivedIssue {
                fqdn: fqdn.to_string(),
                hostname: host.clone(),
                kind: "poe-pse-down".to_string(),
                subject,
                severity: SEV_BAD.to_string(),
                title: "PoE power supply not operational".to_string(),
                description: format!("{} PSE {} is not on", host, budget.group),
                subject_label,
                detail: vec![pair("state", "off")],
            });
            continue;
        }

        if budget.total_w <= 0 {
            continue;
        }
        let utilization = (budget.consumed_w * 100 / budget.total_w).clamp(0, 100);
        let warn_at = budget.threshold_pct.unwrap_or(POE_DEFAULT_WARN_PCT);
        if utilization < warn_at {
            continue;
        }
        let severity = if utilization >= POE_CRITICAL_PCT { SEV_BAD } else { SEV_WARN };
        let remaining = (budget.total_w - budget.consumed_w).max(0);
        let mut detail = vec![
            pair("utilization", format!("{}%", utilization)),
            pair("consumed", format!("{} W", budget.consumed_w)),
            pair("total budget", format!("{} W", budget.total_w)),
            pair("remaining", format!("{} W", remaining)),
        ];
        if let Some(threshold) = budget.threshold_pct {
            detail.push(pair("threshold", format!("{}%", threshold)));
        }
        out.push(DerivedIssue {
            fqdn: fqdn.to_string(),
            hostname: host.clone(),
            kind: "poe-budget".to_string(),
            subject,
            severity: severity.to_string(),
            title: "PoE budget near capacity".to_string(),
            description: format!("{} PSE {} at {}% of its power budget", host, budget.group, utilization),
            subject_label,
            detail,
        });
    }
    out
}

// --- Interface health -----------------------------------------------------

// Splits a tripped interface-health summary into one issue per active signal
// (so each can be acknowledged independently). Nothing is emitted when the
// summary's severity is None (no signal crossed its configured threshold).
pub fn interface_health_issues(fqdn: &str, ifindex: i32, iface_name: &str, h: &json::ApiInterfaceHealth) -> Vec<DerivedIssue> {
    if h.severity.is_none() {
        return Vec::new();
    }
    let host = hostname_of(fqdn);
    let mk = |kind: &str, severity: &str, title: &str, description: String, detail: Vec<(String, String)>| DerivedIssue {
        fqdn: fqdn.to_string(),
        hostname: host.clone(),
        kind: kind.to_string(),
        subject: ifindex.to_string(),
        severity: severity.to_string(),
        title: title.to_string(),
        description,
        subject_label: Some(iface_name.to_string()),
        detail,
    };

    let mut out = Vec::new();
    if h.flap_count > 0 {
        out.push(mk(
            "iface-flapping",
            SEV_BAD,
            "Interface flapping",
            format!("{} {} flapped {} time(s) in the last {}s", host, iface_name, h.flap_count, h.flap_window_secs),
            vec![
                pair("flap count", h.flap_count.to_string()),
                pair("last flap", h.last_flap_secs_ago.map(|s| format!("{}s ago", s)).unwrap_or_else(|| "—".to_string())),
                pair("window", format!("{}s", h.flap_window_secs)),
            ],
        ));
    }
    if h.stale {
        out.push(mk(
            "iface-stale",
            SEV_BAD,
            "Interface not polling",
            format!("{} {} has stopped answering polls", host, iface_name),
            vec![pair("stale", "yes")],
        ));
    }
    let errors = h.in_errors + h.out_errors;
    if errors > 0 {
        out.push(mk(
            "iface-errors",
            SEV_WARN,
            "Interface errors",
            format!("{} {} saw {} error(s) in the last {}s", host, iface_name, errors, h.counter_window_secs),
            vec![
                pair("in errors", h.in_errors.to_string()),
                pair("out errors", h.out_errors.to_string()),
                pair("window", format!("{}s", h.counter_window_secs)),
            ],
        ));
    }
    if h.discards > 0 {
        out.push(mk(
            "iface-discards",
            SEV_WARN,
            "Interface discards",
            format!("{} {} discarded {} packet(s) in the last {}s", host, iface_name, h.discards, h.counter_window_secs),
            vec![
                pair("discards", h.discards.to_string()),
                pair("window", format!("{}s", h.counter_window_secs)),
            ],
        ));
    }
    if h.high_utilization {
        let peak = h.peak_utilization_pct.map(|p| format!("{:.0}%", p)).unwrap_or_else(|| "—".to_string());
        out.push(mk(
            "iface-high-util",
            SEV_WARN,
            "High utilization",
            format!("{} {} peaked at {} in the last {}s", host, iface_name, peak, h.util_window_secs),
            vec![
                pair("peak utilization", peak),
                pair("window", format!("{}s", h.util_window_secs)),
            ],
        ));
    }
    if h.speed_change_count > 0 {
        let last = h
            .last_speed_change
            .map(|(from, to)| match from {
                Some(f) => format!("{} → {} Mbps", f, to),
                None => format!("{} Mbps", to),
            })
            .unwrap_or_else(|| "—".to_string());
        out.push(mk(
            "iface-speed",
            SEV_WARN,
            "Speed renegotiation",
            format!("{} {} renegotiated speed ({})", host, iface_name, last),
            vec![
                pair("changes", h.speed_change_count.to_string()),
                pair("last change", last),
            ],
        ));
    }
    out
}

// --- STP ------------------------------------------------------------------

// Structural spanning-tree anomalies for one VLAN's computed tree. Routine
// blocked (alternate/backup) ports are deliberately NOT issues — blocking is
// STP working correctly; only genuine anomalies are surfaced.
pub fn stp_tree_issues(tree: &json::ApiStpTree) -> Vec<DerivedIssue> {
    let vlan = tree.vlan;
    let vlan_label = format!("VLAN {}", vlan);
    let subject = format!("vlan{}", vlan);
    let mut out = Vec::new();

    for flag in tree.flags.iter() {
        // Flags are "code" or "code:<fqdn>" (multiple-root-ports).
        let (code, flag_fqdn) = match flag.split_once(':') {
            Some((code, fqdn)) => (code, Some(fqdn.to_string())),
            None => (flag.as_str(), None),
        };
        let device = flag_fqdn.clone().unwrap_or_default();
        let (severity, title, description) = match code {
            "no-root" => (SEV_BAD, "STP: no root bridge", format!("VLAN {} has STP nodes but no elected root bridge", vlan)),
            "multiple-roots" => (SEV_BAD, "STP: multiple roots", format!("VLAN {} has more than one root bridge", vlan)),
            "cycle" => (SEV_BAD, "STP: cycle", format!("VLAN {} spanning tree contains a cycle", vlan)),
            "multiple-root-ports" => (
                SEV_WARN,
                "STP: multiple root ports",
                format!("{} has multiple root ports on VLAN {}", hostname_of(&device), vlan),
            ),
            _ => (SEV_WARN, "STP anomaly", format!("VLAN {}: {}", vlan, flag)),
        };
        out.push(DerivedIssue {
            fqdn: device.clone(),
            hostname: hostname_of(&device),
            kind: format!("stp-flag:{}", code),
            subject: subject.clone(),
            severity: severity.to_string(),
            title: title.to_string(),
            description,
            subject_label: Some(vlan_label.clone()),
            detail: vec![pair("vlan", vlan.to_string()), pair("flag", flag.clone())],
        });
    }

    for node in tree.nodes.iter() {
        let host = hostname_of(&node.fqdn);
        if node.root_mismatch {
            out.push(DerivedIssue {
                fqdn: node.fqdn.clone(),
                hostname: host.clone(),
                kind: "stp-root-mismatch".to_string(),
                subject: subject.clone(),
                severity: SEV_BAD.to_string(),
                title: "STP root mismatch".to_string(),
                description: format!("{} disagrees about the VLAN {} root bridge", host, vlan),
                subject_label: Some(vlan_label.clone()),
                detail: vec![
                    pair("vlan", vlan.to_string()),
                    pair(
                        "reported root",
                        node.reported.as_ref().and_then(|b| b.root_mac.clone()).unwrap_or_else(|| "—".to_string()),
                    ),
                ],
            });
        }
        if node.orphan {
            out.push(DerivedIssue {
                fqdn: node.fqdn.clone(),
                hostname: host.clone(),
                kind: "stp-orphan".to_string(),
                subject: subject.clone(),
                severity: SEV_WARN.to_string(),
                title: "STP orphan".to_string(),
                description: format!("{} has a root port on VLAN {} but its upstream is unresolved", host, vlan),
                subject_label: Some(vlan_label.clone()),
                detail: vec![
                    pair("vlan", vlan.to_string()),
                    pair("root port", node.root_port_interface_name.clone().unwrap_or_else(|| "—".to_string())),
                ],
            });
        }
    }
    out
}

// --- LAG / port-channel ---------------------------------------------------

fn lag_severity(code: &str) -> &'static str {
    match code {
        // Active data-path faults.
        "member-not-bundled" | "member-no-lacp-partner" | "members-wired-to-different-devices" => SEV_BAD,
        _ => SEV_WARN,
    }
}

fn lag_human(code: &str) -> &'static str {
    match code {
        "not-lacp" => "not running LACP",
        "single-member" => "has only one member",
        "member-no-lacp-partner" => "member has no LACP partner",
        "member-not-bundled" => "member is not bundled",
        "members-report-different-partners" => "members report different LACP partners",
        "members-wired-to-different-devices" => "members are wired to different devices",
        "far-end-lag-not-found" => "far-end aggregate not found",
        "far-end-member-count-mismatch" => "far-end member count mismatch",
        _ => "aggregation warning",
    }
}

pub fn port_channel_issues(fqdn: &str, pc: &json::ApiPortChannel) -> Vec<DerivedIssue> {
    let host = hostname_of(fqdn);
    let agg_name = pc.name.clone().unwrap_or_else(|| format!("ifIndex {}", pc.ifindex));
    pc.warnings
        .iter()
        .map(|warning| {
            // Warnings are "code" or "code:<detail>" (member name or protocol).
            let (code, suffix) = match warning.split_once(':') {
                Some((code, rest)) => (code, Some(rest.to_string())),
                None => (warning.as_str(), None),
            };
            let human = lag_human(code);
            let description = match suffix.as_ref() {
                Some(s) => format!("{} {} {} ({})", host, agg_name, human, s),
                None => format!("{} {} {}", host, agg_name, human),
            };
            let mut detail = vec![
                pair("port-channel", agg_name.clone()),
                pair("protocol", pc.protocol.clone()),
                pair("warning", warning.clone()),
            ];
            if let Some(s) = suffix.as_ref() {
                detail.push(pair("detail", s.clone()));
            }
            DerivedIssue {
                fqdn: fqdn.to_string(),
                hostname: host.clone(),
                kind: format!("lag:{}", code),
                // Same code can hit several aggregates/members on one device;
                // key on the aggregate ifindex plus the detail suffix.
                subject: format!("{}:{}", pc.ifindex, suffix.clone().unwrap_or_default()),
                severity: lag_severity(code).to_string(),
                title: format!("Port-channel: {}", human),
                description,
                subject_label: Some(agg_name.clone()),
                detail,
            }
        })
        .collect()
}

// --- Tracker --------------------------------------------------------------

#[derive(Clone, Debug)]
struct Occurrence {
    first_seen: u64,
    last_seen: u64,
}

pub struct IssueTracker {
    seen: HashMap<String, Occurrence>,
    // Keep a cleared issue's occurrence for this long before forgetting it, so a
    // single missed scan (a transient blip) doesn't reset first_seen and
    // needlessly re-alert an acknowledged issue.
    grace_ms: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackedIssue {
    pub issue: DerivedIssue,
    pub first_seen: u64,
    pub last_seen: u64,
}

impl IssueTracker {
    pub fn new(grace_ms: u64) -> IssueTracker {
        IssueTracker { seen: HashMap::new(), grace_ms }
    }

    // Fold the current derivation into the tracker: stamp/refresh
    // first_seen/last_seen, forget keys unseen past the grace window, and return
    // the active issues enriched with their timestamps.
    pub fn reconcile(&mut self, now: u64, derived: Vec<DerivedIssue>) -> Vec<TrackedIssue> {
        let mut result = Vec::with_capacity(derived.len());
        let mut current_keys: HashSet<String> = HashSet::new();
        for issue in derived.into_iter() {
            let key = issue.issue_key();
            current_keys.insert(key.clone());
            let occ = self.seen.entry(key).or_insert(Occurrence { first_seen: now, last_seen: now });
            occ.last_seen = now;
            result.push(TrackedIssue { issue, first_seen: occ.first_seen, last_seen: occ.last_seen });
        }
        let cutoff = now.saturating_sub(self.grace_ms);
        self.seen.retain(|key, occ| current_keys.contains(key) || occ.last_seen >= cutoff);
        result
    }

    // First-seen timestamp of the currently-tracked occurrence of `key`.
    pub fn first_seen_of(&self, key: &str) -> Option<u64> {
        self.seen.get(key).map(|o| o.first_seen)
    }

    // Every key the tracker still remembers (active plus grace-retained). Used
    // to prune orphaned ack rows without dropping one during the grace window.
    pub fn known_keys(&self) -> HashSet<String> {
        self.seen.keys().cloned().collect()
    }

    // Seed an occurrence's first_seen if the key is not already tracked. Called
    // once at startup from the persisted acks so that a nexus restart, unlike a
    // genuine clear-then-recur, does NOT reset first_seen and re-alert a
    // still-active acknowledged issue. Must run before any reconcile: reconcile
    // only inserts (never overwrites) first_seen, so a later reconcile keeps the
    // seeded value while an issue stays active, and drops it once the fault
    // clears — at which point the ack becomes an orphan and is pruned.
    // `now` is stamped as last_seen (not first_seen) so the seeded occurrence
    // survives the grace window from startup, giving a still-active fault time to
    // re-materialize in the collectors after a restart before the ack is pruned.
    pub fn seed(&mut self, key: String, first_seen: u64, now: u64) {
        self.seen.entry(key).or_insert(Occurrence { first_seen, last_seen: now });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collectors::poe::PoeBudget;

    fn budget(total_w: i64, consumed_w: i64, oper_on: bool, threshold_pct: Option<i64>) -> PoeBudget {
        PoeBudget { group: 1, total_w, consumed_w, oper_on, threshold_pct }
    }

    #[test]
    fn poe_budget_below_default_threshold_is_silent() {
        // 10/124 W ~= 8% (live ticket-sw2), well under the 85% default.
        assert!(poe_budget_issues("sw1.example.com", &[budget(124, 10, true, None)]).is_empty());
    }

    #[test]
    fn poe_budget_over_default_threshold_warns() {
        let issues = poe_budget_issues("sw1.example.com", &[budget(124, 110, true, None)]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "poe-budget");
        assert_eq!(issues[0].severity, SEV_WARN);
        assert_eq!(issues[0].subject, "1");
        assert_eq!(issues[0].subject_label.as_deref(), Some("PSE 1"));
        // 110/124 = 88%.
        assert!(issues[0].detail.iter().any(|(k, v)| k == "utilization" && v == "88%"));
        assert!(issues[0].detail.iter().any(|(k, v)| k == "remaining" && v == "14 W"));
    }

    #[test]
    fn poe_budget_past_critical_ceiling_is_bad() {
        // 120/124 = 96% >= 95% critical ceiling.
        let issues = poe_budget_issues("sw1.example.com", &[budget(124, 120, true, None)]);
        assert_eq!(issues[0].severity, SEV_BAD);
    }

    #[test]
    fn poe_budget_honors_configured_threshold() {
        // 70/124 = 56%; under the 85% default but over a configured 50%.
        let issues = poe_budget_issues("sw1.example.com", &[budget(124, 70, true, Some(50))]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, SEV_WARN);
        assert!(issues[0].detail.iter().any(|(k, v)| k == "threshold" && v == "50%"));
    }

    #[test]
    fn poe_pse_not_operational_is_bad_and_skips_budget() {
        let issues = poe_budget_issues("sw1.example.com", &[budget(124, 200, false, None)]);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "poe-pse-down");
        assert_eq!(issues[0].severity, SEV_BAD);
    }

    #[test]
    fn poe_budget_one_issue_per_pse_group() {
        let budgets = vec![
            budget(124, 118, true, None),
            PoeBudget { group: 2, total_w: 124, consumed_w: 5, oper_on: true, threshold_pct: None },
        ];
        let issues = poe_budget_issues("sw1.example.com", &budgets);
        // Only group 1 is near budget; group 2 is quiet.
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].subject, "1");
    }

    fn health(severity: Option<&str>) -> json::ApiInterfaceHealth {
        json::ApiInterfaceHealth {
            severity: severity.map(|s| s.to_string()),
            flap_count: 0,
            last_flap_secs_ago: None,
            in_errors: 0,
            out_errors: 0,
            discards: 0,
            speed_change_count: 0,
            last_speed_change: None,
            peak_utilization_pct: None,
            high_utilization: false,
            rx_bps_avg: None,
            tx_bps_avg: None,
            stale: false,
            counter_window_secs: 300,
            flap_window_secs: 600,
            util_window_secs: 300,
            throughput_window_secs: 60,
        }
    }

    #[test]
    fn device_down_only_when_false() {
        assert!(device_down_issue("sw1.example.com", Some(true), None).is_none());
        assert!(device_down_issue("sw1.example.com", None, None).is_none());
        let issue = device_down_issue("sw1.example.com", Some(false), Some(42)).unwrap();
        assert_eq!(issue.kind, "device-down");
        assert_eq!(issue.severity, SEV_BAD);
        assert_eq!(issue.hostname, "sw1");
        assert_eq!(issue.issue_key(), "sw1.example.com|device-down|");
    }

    #[test]
    fn healthy_interface_yields_nothing() {
        assert!(interface_health_issues("sw1.example.com", 1, "Gi1/0/1", &health(None)).is_empty());
    }

    #[test]
    fn interface_signals_split_into_one_issue_each() {
        let mut h = health(Some("bad"));
        h.flap_count = 3;
        h.last_flap_secs_ago = Some(12);
        h.in_errors = 5;
        h.discards = 7;
        h.high_utilization = true;
        h.peak_utilization_pct = Some(95.0);
        h.speed_change_count = 1;
        h.last_speed_change = Some((Some(1000), 100));
        let issues = interface_health_issues("sw1.example.com", 10001, "Gi1/0/1", &h);
        let kinds: HashSet<&str> = issues.iter().map(|i| i.kind.as_str()).collect();
        assert!(kinds.contains("iface-flapping"));
        assert!(kinds.contains("iface-errors"));
        assert!(kinds.contains("iface-discards"));
        assert!(kinds.contains("iface-high-util"));
        assert!(kinds.contains("iface-speed"));
        // Flapping is Bad; errors/discards/util/speed are Warn.
        let flap = issues.iter().find(|i| i.kind == "iface-flapping").unwrap();
        assert_eq!(flap.severity, SEV_BAD);
        assert_eq!(flap.subject, "10001");
        assert_eq!(flap.issue_key(), "sw1.example.com|iface-flapping|10001");
        assert_eq!(issues.iter().find(|i| i.kind == "iface-errors").unwrap().severity, SEV_WARN);
    }

    #[test]
    fn stale_interface_is_bad() {
        let mut h = health(Some("bad"));
        h.stale = true;
        let issues = interface_health_issues("sw1.example.com", 1, "Gi1", &h);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "iface-stale");
        assert_eq!(issues[0].severity, SEV_BAD);
    }

    #[test]
    fn stp_flags_and_node_anomalies() {
        let tree = json::ApiStpTree {
            vlan: 10,
            roots: vec![],
            nodes: vec![
                json::ApiStpNode {
                    fqdn: "dist1.example.com".to_string(),
                    depth: 0,
                    parent: None,
                    parent_interface: None,
                    root_port_interface_name: Some("Te1/1/1".to_string()),
                    root_port_state: None,
                    path_cost: None,
                    reported: None,
                    root_mismatch: true,
                    orphan: false,
                },
                json::ApiStpNode {
                    fqdn: "access1.example.com".to_string(),
                    depth: 1,
                    parent: None,
                    parent_interface: None,
                    root_port_interface_name: None,
                    root_port_state: None,
                    path_cost: None,
                    reported: None,
                    root_mismatch: false,
                    orphan: true,
                },
            ],
            blocked_links: vec![],
            flags: vec!["no-root".to_string(), "multiple-root-ports:core1.example.com".to_string()],
        };
        let issues = stp_tree_issues(&tree);
        let by_kind: HashMap<&str, &DerivedIssue> = issues.iter().map(|i| (i.kind.as_str(), i)).collect();
        assert_eq!(by_kind["stp-flag:no-root"].severity, SEV_BAD);
        assert_eq!(by_kind["stp-flag:no-root"].fqdn, ""); // vlan-level
        assert_eq!(by_kind["stp-flag:multiple-root-ports"].fqdn, "core1.example.com");
        assert_eq!(by_kind["stp-root-mismatch"].fqdn, "dist1.example.com");
        assert_eq!(by_kind["stp-root-mismatch"].severity, SEV_BAD);
        assert_eq!(by_kind["stp-orphan"].fqdn, "access1.example.com");
        assert_eq!(by_kind["stp-orphan"].severity, SEV_WARN);
        // Distinct subject keeps per-vlan flags unique.
        assert_eq!(by_kind["stp-flag:no-root"].issue_key(), "|stp-flag:no-root|vlan10");
    }

    #[test]
    fn port_channel_warnings_map_to_issues() {
        let pc = json::ApiPortChannel {
            ifindex: 5001,
            name: Some("Po1".to_string()),
            up: Some(true),
            protocol: "lacp".to_string(),
            partner_system_id: None,
            members: vec![],
            warnings: vec!["single-member".to_string(), "member-not-bundled:Te1/0/1".to_string()],
        };
        let issues = port_channel_issues("dist1.example.com", &pc);
        assert_eq!(issues.len(), 2);
        let single = issues.iter().find(|i| i.kind == "lag:single-member").unwrap();
        assert_eq!(single.severity, SEV_WARN);
        assert_eq!(single.subject, "5001:");
        let notb = issues.iter().find(|i| i.kind == "lag:member-not-bundled").unwrap();
        assert_eq!(notb.severity, SEV_BAD);
        assert_eq!(notb.subject, "5001:Te1/0/1");
        assert_eq!(notb.issue_key(), "dist1.example.com|lag:member-not-bundled|5001:Te1/0/1");
    }

    #[test]
    fn tracker_keeps_first_seen_stable_then_resets_after_clear() {
        let mut tracker = IssueTracker::new(0); // no grace: clear forgets immediately
        let issue = device_down_issue("sw1.example.com", Some(false), None).unwrap();
        let key = issue.issue_key();

        let r0 = tracker.reconcile(1000, vec![issue.clone()]);
        assert_eq!(r0[0].first_seen, 1000);
        assert_eq!(tracker.first_seen_of(&key), Some(1000));

        // Still active later: first_seen stays put, last_seen advances.
        let r1 = tracker.reconcile(5000, vec![issue.clone()]);
        assert_eq!(r1[0].first_seen, 1000);
        assert_eq!(r1[0].last_seen, 5000);

        // Cleared: tracker forgets it (grace 0).
        tracker.reconcile(6000, vec![]);
        assert_eq!(tracker.first_seen_of(&key), None);

        // Recurs: fresh first_seen — this is what makes a stale ack re-alert.
        let r3 = tracker.reconcile(9000, vec![issue.clone()]);
        assert_eq!(r3[0].first_seen, 9000);
    }

    #[test]
    fn seed_preserves_first_seen_across_a_restart() {
        // Simulates: acked while active (first_seen 1000), nexus restarts at
        // 9000, fault still active. Seeding from the persisted ack keeps
        // first_seen at 1000 so the ack still matches (no spurious re-alert).
        let mut tracker = IssueTracker::new(60_000);
        let issue = device_down_issue("sw1.example.com", Some(false), None).unwrap();
        let key = issue.issue_key();
        tracker.seed(key.clone(), 1000, 9000);
        let r = tracker.reconcile(9000, vec![issue.clone()]);
        assert_eq!(r[0].first_seen, 1000, "restart must not reset first_seen for a seeded, still-active issue");
    }

    #[test]
    fn seed_does_not_override_a_runtime_recurrence() {
        // Seeding only happens at startup. Within a running process a genuine
        // clear-then-recur must still get a fresh first_seen.
        let mut tracker = IssueTracker::new(0);
        let issue = device_down_issue("sw1.example.com", Some(false), None).unwrap();
        tracker.reconcile(1000, vec![issue.clone()]);
        tracker.reconcile(2000, vec![]); // clears (grace 0 forgets)
        let r = tracker.reconcile(5000, vec![issue.clone()]);
        assert_eq!(r[0].first_seen, 5000);
    }

    #[test]
    fn tracker_grace_window_survives_a_missed_scan() {
        let mut tracker = IssueTracker::new(60_000);
        let issue = device_down_issue("sw1.example.com", Some(false), None).unwrap();
        let key = issue.issue_key();
        tracker.reconcile(1000, vec![issue.clone()]);
        // One scan misses it, but within grace — occurrence is retained.
        tracker.reconcile(30_000, vec![]);
        assert_eq!(tracker.first_seen_of(&key), Some(1000));
        // Back within grace: original first_seen preserved.
        let r = tracker.reconcile(40_000, vec![issue.clone()]);
        assert_eq!(r[0].first_seen, 1000);
    }
}
