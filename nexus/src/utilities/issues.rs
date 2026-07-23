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
use crate::models::json::{ApiIssueDetail, ApiIssueDetailValue};

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

// --- Detail row constructors ---------------------------------------------
// `pair` builds a plain-text row (the default every signal uses); the typed
// helpers below let an issue surface a MAC, device link, interface, verdict or
// hyperlink the UI can render richly.

fn pair(k: &str, v: impl Into<String>) -> ApiIssueDetail {
    ApiIssueDetail { label: k.to_string(), value: ApiIssueDetailValue::Text { text: v.into() } }
}

fn mac_row(k: &str, mac: String, monitored: bool) -> ApiIssueDetail {
    ApiIssueDetail { label: k.to_string(), value: ApiIssueDetailValue::Mac { mac, monitored } }
}

fn device_row(k: &str, fqdn: String) -> ApiIssueDetail {
    let hostname = hostname_of(&fqdn);
    ApiIssueDetail { label: k.to_string(), value: ApiIssueDetailValue::Device { fqdn, hostname } }
}

fn iface_row(k: &str, name: String, state: Option<String>) -> ApiIssueDetail {
    ApiIssueDetail { label: k.to_string(), value: ApiIssueDetailValue::Interface { name, state } }
}

fn verdict_row(k: &str, text: impl Into<String>, tone: &str) -> ApiIssueDetail {
    ApiIssueDetail { label: k.to_string(), value: ApiIssueDetailValue::Verdict { text: text.into(), tone: tone.to_string() } }
}

fn link_row(k: &str, text: impl Into<String>, href: impl Into<String>) -> ApiIssueDetail {
    ApiIssueDetail { label: k.to_string(), value: ApiIssueDetailValue::Link { text: text.into(), href: href.into() } }
}

// Compact human duration from seconds: "154d", "3h 5m", "45m", "30s".
fn human_secs(secs: i64) -> String {
    let s = secs.max(0);
    if s < 60 {
        format!("{}s", s)
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d", s / 86400)
    }
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
    pub detail: Vec<ApiIssueDetail>,
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
    let mk = |kind: &str, severity: &str, title: &str, description: String, detail: Vec<ApiIssueDetail>| DerivedIssue {
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
    let stp_tree_link = link_row("spanning tree", format!("VLAN {} tree", vlan), format!("/stp?vlan={}", vlan));

    for flag in tree.flags.iter() {
        // Flags are "code" or "code:<fqdn>" (multiple-root-ports).
        let (code, flag_fqdn) = match flag.split_once(':') {
            Some((code, fqdn)) => (code, Some(fqdn.to_string())),
            None => (flag.as_str(), None),
        };
        let device = flag_fqdn.clone().unwrap_or_default();
        let (mut severity, title, description) = match code {
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
        // Enrich "multiple roots" with the competing claims and whether they
        // are actually connected. Roots that share no path are most likely
        // independent segments, not a fault — downgrade those to a warning.
        let detail = match (code, tree.multiple_roots_detail.as_ref()) {
            ("multiple-roots", Some(d)) => {
                // Critical only when the roots genuinely share a VLAN path (a
                // link carrying the VLAN on both ends) yet still disagree — a
                // real convergence failure. An asymmetric trunk, a link that
                // doesn't carry the VLAN, or no link at all is isolation, not a
                // loop: downgrade to a warning.
                let shared_vlan_path = d.adjacent == Some(true)
                    && d.connecting_link.as_ref().map(|l| l.a_has_vlan && l.b_has_vlan).unwrap_or(false);
                if !shared_vlan_path {
                    severity = SEV_WARN;
                }
                multiple_roots_detail(d, &stp_tree_link, vlan)
            }
            _ => vec![pair("vlan", vlan.to_string()), pair("flag", flag.clone())],
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
            detail,
        });
    }

    for node in tree.nodes.iter() {
        let host = hostname_of(&node.fqdn);
        if node.root_mismatch {
            // A disagreement whose "winning" root is a *superior, unmonitored*
            // bridge is not a fault on this device — jaspy simply does not poll
            // the real root. Downgrade it from a critical mismatch to a warn
            // that names the situation, and don't pin blame on the reporter.
            let unmonitored_root = node
                .root_mismatch_detail
                .as_ref()
                .map(|d| d.reported_root_superior == Some(true) && !d.reported_root_monitored)
                .unwrap_or(false);
            let (kind, severity, title, description) = if unmonitored_root {
                (
                    "stp-unmonitored-root",
                    SEV_WARN,
                    "STP root not monitored",
                    format!(
                        "VLAN {}'s root bridge is not monitored by jaspy — {} sees a superior root upstream",
                        vlan, host
                    ),
                )
            } else {
                (
                    "stp-root-mismatch",
                    SEV_BAD,
                    "STP root mismatch",
                    format!("{} disagrees about the VLAN {} root bridge", host, vlan),
                )
            };
            out.push(DerivedIssue {
                fqdn: node.fqdn.clone(),
                hostname: host.clone(),
                kind: kind.to_string(),
                subject: subject.clone(),
                severity: severity.to_string(),
                title: title.to_string(),
                description,
                subject_label: Some(vlan_label.clone()),
                detail: root_mismatch_detail(node, &stp_tree_link),
            });
        }
        if node.orphan {
            let mut detail = Vec::new();
            if let Some(name) = node.root_port_interface_name.clone() {
                detail.push(iface_row("root port", name, node.root_port_state.clone()));
            } else {
                detail.push(pair("root port", "—"));
            }
            if let Some(cost) = node.path_cost {
                detail.push(pair("path cost", cost.to_string()));
            }
            detail.push(verdict_row(
                "upstream",
                "unresolved — the neighbour on the root port is not monitored (or has no discovered link)",
                "warn",
            ));
            detail.push(stp_tree_link.clone());
            out.push(DerivedIssue {
                fqdn: node.fqdn.clone(),
                hostname: host.clone(),
                kind: "stp-orphan".to_string(),
                subject: subject.clone(),
                severity: SEV_WARN.to_string(),
                title: "STP orphan".to_string(),
                description: format!("{} has a root port on VLAN {} but its upstream is unresolved", host, vlan),
                subject_label: Some(vlan_label.clone()),
                detail,
            });
        }
    }
    out
}

// The expanded detail for a root-mismatch: both sides of the disagreement,
// their bridge priorities, whether the reported root is even monitored, and a
// verdict on which claim STP would actually elect — everything an admin needs
// to triage without leaving the Issues page.
// The expanded detail for a "multiple roots" flag: each bridge claiming root
// (with its bridge ID, and the preferred/lowest marked), whether the claimants
// are actually connected, and a grounded verdict — so the admin can tell a real
// split brain from independently-monitored segments.
fn multiple_roots_detail(d: &json::ApiStpMultipleRootsDetail, stp_tree_link: &ApiIssueDetail, vlan: i64) -> Vec<ApiIssueDetail> {
    let mut detail = Vec::new();
    for claim in d.roots.iter() {
        detail.push(ApiIssueDetail {
            label: if claim.preferred { "claimed root (preferred)".to_string() } else { "claimed root".to_string() },
            value: ApiIssueDetailValue::StpRoot {
                fqdn: claim.fqdn.clone(),
                hostname: claim.hostname.clone(),
                mac: claim.mac.clone(),
                priority: claim.priority,
                preferred: claim.preferred,
            },
        });
    }
    match (d.adjacent, d.connecting_link.as_ref()) {
        (Some(true), Some(link)) => {
            let a = format!("{} {}", hostname_of(&link.a_fqdn), link.a_interface);
            let b = format!("{} {}", hostname_of(&link.b_fqdn), link.b_interface);
            detail.push(pair("roots connected", format!("{} ↔ {}", a, b)));
            match (link.a_has_vlan, link.b_has_vlan) {
                (true, true) => detail.push(verdict_row(
                    "verdict",
                    format!("VLAN {} is on both ends of this link yet the bridges have not converged — a real spanning-tree failure (loop / black-hole risk). Check BPDU flow across the link.", vlan),
                    "bad",
                )),
                (false, false) => detail.push(verdict_row(
                    "verdict",
                    format!("the roots are linked but VLAN {} is on neither end of that link — they are separate VLAN {} segments, not a shared tree (not a loop)", vlan, vlan),
                    "neutral",
                )),
                // Asymmetric trunk: VLAN present on one end only.
                (a_ok, _) => {
                    let (present, missing) = if a_ok { (&a, &b) } else { (&b, &a) };
                    detail.push(verdict_row(
                        "verdict",
                        format!("asymmetric trunk — VLAN {} is allowed on {} but not on {}, so the two roots cannot merge. Add VLAN {} to {}, or ignore if the split is intentional.", vlan, present, missing, vlan, missing),
                        "warn",
                    ));
                }
            }
        }
        (Some(false), _) | (Some(true), None) => {
            detail.push(pair("roots connected", "no monitored link between them"));
            detail.push(verdict_row(
                "verdict",
                "no path between these roots in the discovered topology — most likely independent L2 segments jaspy monitors separately, not a fault",
                "neutral",
            ));
        }
        (None, _) => {
            detail.push(verdict_row("verdict", "could not determine whether these roots are connected", "neutral"));
        }
    }
    detail.push(stp_tree_link.clone());
    detail
}

fn root_mismatch_detail(node: &json::ApiStpNode, stp_tree_link: &ApiIssueDetail) -> Vec<ApiIssueDetail> {
    let mut detail = Vec::new();
    let Some(d) = node.root_mismatch_detail.as_ref() else {
        // No context computed (shouldn't happen for a real mismatch): fall back
        // to the bare reported MAC.
        detail.push(mac_row(
            "reported root",
            node.reported.as_ref().and_then(|b| b.root_mac.clone()).unwrap_or_else(|| "—".to_string()),
            false,
        ));
        return detail;
    };

    // This node's claim.
    detail.push(mac_row(
        "reported root",
        d.reported_root_mac.clone().unwrap_or_else(|| "—".to_string()),
        d.reported_root_monitored,
    ));
    if let Some(p) = d.reported_root_priority {
        detail.push(pair("reported root priority", p.to_string()));
    }
    // The bridge jaspy elected as root for the VLAN — the other side of the
    // disagreement. When the reported root is the superior one, this pick is
    // the isolated/stale bridge, so label it as jaspy's structural pick rather
    // than "the root" to avoid implying it is authoritative.
    detail.push(device_row("jaspy's elected root", d.computed_root_fqdn.clone()));
    if let Some(mac) = d.computed_root_mac.clone() {
        detail.push(mac_row("elected root bridge", mac, true));
    }
    if let Some(p) = d.computed_root_priority {
        detail.push(pair("elected root priority", p.to_string()));
    }

    // The verdict: which bridge ID STP actually prefers, and — crucially —
    // whether this device is at fault. Coloured by how alarming it is.
    let verdict = match (d.reported_root_superior, d.reported_root_monitored) {
        (Some(true), false) => Some(verdict_row(
            "verdict",
            "this device sees a superior root (lower bridge ID) that jaspy does not monitor — that off-fleet bridge, not jaspy's elected root, is the VLAN's real root. This device is not at fault; jaspy just doesn't poll the actual root.",
            "warn",
        )),
        (Some(true), true) => Some(verdict_row(
            "verdict",
            "the reported root has a superior bridge ID and is a monitored device — jaspy's elected root should not be root. Real disagreement between two monitored bridges.",
            "bad",
        )),
        (Some(false), _) => Some(verdict_row(
            "verdict",
            "jaspy's elected root has the superior bridge ID; this node reports a weaker root — likely stale or partitioned STP data on this device.",
            "neutral",
        )),
        (None, _) => None,
    };
    if let Some(v) = verdict {
        detail.push(v);
    }

    // Where the disagreement enters this switch.
    if let Some(name) = node.root_port_interface_name.clone() {
        detail.push(iface_row("entered via", name, node.root_port_state.clone()));
    }
    if node.orphan {
        detail.push(verdict_row("upstream", "unresolved — see the STP orphan on this VLAN", "warn"));
    }
    if let Some(secs) = node.reported.as_ref().and_then(|b| b.time_since_topology_change_secs) {
        detail.push(pair("last topology change", format!("{} ago", human_secs(secs))));
    }
    detail.push(stp_tree_link.clone());
    detail
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

// `root_depth` maps fqdn -> its shallowest hop distance from a computed STP
// root (across all VLANs). Used only for the down-uplink verdict's best-effort
// guess at which cable end came loose; empty when no STP data is available.
pub fn port_channel_issues(
    fqdn: &str,
    pc: &json::ApiPortChannel,
    root_depth: &HashMap<String, i64>,
) -> Vec<DerivedIssue> {
    let host = hostname_of(fqdn);
    let agg_name = pc.name.clone().unwrap_or_else(|| format!("ifIndex {}", pc.ifindex));
    let mut issues: Vec<DerivedIssue> = Vec::new();

    // The common "a cable fell out of one of two LACP uplinks" failure: one (or
    // more) member is operationally down while the bundle still carries at least
    // one working member. Surface it as a focused, actionable verdict instead of
    // the generic per-member "not bundled" warning it would otherwise produce.
    let down: Vec<&json::ApiPortChannelMember> =
        pc.members.iter().filter(|m| m.up == Some(false)).collect();
    // "Redundancy remains" means at least one other member is actually carrying
    // traffic (bundled and up). This deliberately excludes a bundle whose only
    // survivor is itself unhealthy (e.g. a misconfigured member not bundling) —
    // that is a different, already-warned problem, not a clean "one uplink of
    // several fell out".
    let healthy: Vec<&json::ApiPortChannelMember> =
        pc.members.iter().filter(|m| m.bundled && m.up == Some(true)).collect();
    let mut down_member_names: HashSet<String> = HashSet::new();
    if !down.is_empty() && !healthy.is_empty() {
        let down_names: Vec<String> =
            down.iter().filter_map(|m| m.name.clone()).collect();
        for n in &down_names {
            down_member_names.insert(n.clone());
        }
        let names_joined = if down_names.is_empty() {
            "a member".to_string()
        } else {
            down_names.join(", ")
        };
        let count = down.len();
        let (count_word, noun, cable) = if count == 1 {
            ("one".to_string(), "uplink", "cable is")
        } else {
            (count.to_string(), "uplinks", "cables are")
        };

        // Best-effort guess at the loose end: the switch further from the STP
        // root (typically the access-layer / leaf side) is the more likely spot
        // for an accidentally disturbed cable. We can never be certain which end
        // is loose, so the wording stays a hint.
        let peer_fqdn = down.iter().find_map(|m| m.connected_to.as_ref().map(|c| c.fqdn.clone()));
        let guess = match (root_depth.get(fqdn), peer_fqdn.as_ref().and_then(|p| root_depth.get(p))) {
            (Some(here), Some(there)) if here > there =>
                format!(" The loose end is most likely here — {} sits further from the STP root.", host),
            (Some(here), Some(there)) if here < there => format!(
                " The loose end is most likely at the far end ({}), which sits further from the STP root — but check both.",
                peer_fqdn.as_ref().map(|p| hostname_of(p)).unwrap_or_default()
            ),
            _ => " We can't tell which end is loose — check both, starting with the switch furthest from the STP root.".to_string(),
        };
        let verdict = format!(
            "{} has {} {} ({}) DOWN. Check that the {} firmly attached.{}",
            host, count_word, noun, names_joined, cable, guess
        );

        let mut detail = vec![
            verdict_row("verdict", verdict, "warn"),
            pair("port-channel", agg_name.clone()),
            pair("protocol", pc.protocol.clone()),
        ];
        for m in &down {
            detail.push(iface_row("down member", m.name.clone().unwrap_or_else(|| m.ifindex.to_string()), Some("down".to_string())));
        }
        let up_names: Vec<String> = healthy.iter().filter_map(|m| m.name.clone()).collect();
        if !up_names.is_empty() {
            detail.push(pair("still up", up_names.join(", ")));
        }
        if let Some(p) = peer_fqdn.as_ref() {
            detail.push(device_row("upstream", p.clone()));
        }

        // Sorted names keep the subject (and thus the issue key) stable
        // regardless of member iteration order.
        let mut sorted = down_names.clone();
        sorted.sort();
        issues.push(DerivedIssue {
            fqdn: fqdn.to_string(),
            hostname: host.clone(),
            kind: "lag:member-link-down".to_string(),
            subject: format!("{}:{}", pc.ifindex, sorted.join(",")),
            severity: SEV_WARN.to_string(),
            title: "Port-channel: uplink member down".to_string(),
            description: format!("{} {} {} member ({}) down", host, agg_name, count_word, names_joined),
            subject_label: Some(agg_name.clone()),
            detail,
        });
    }

    for warning in pc.warnings.iter() {
        // Warnings are "code" or "code:<detail>" (member name or protocol).
        let (code, suffix) = match warning.split_once(':') {
            Some((code, rest)) => (code, Some(rest.to_string())),
            None => (warning.as_str(), None),
        };
        // A member the down-uplink verdict already covers surfaces here only as
        // "not bundled" (its link is down, so it cannot bundle) — suppress that
        // duplicate.
        if code == "member-not-bundled" {
            if let Some(s) = suffix.as_ref() {
                if down_member_names.contains(s) {
                    continue;
                }
            }
        }
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
        issues.push(DerivedIssue {
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
        });
    }
    issues
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

    // Plain-text value of the detail row with `label`, if any.
    fn detail_text<'a>(issue: &'a DerivedIssue, label: &str) -> Option<&'a str> {
        issue.detail.iter().find(|d| d.label == label).and_then(|d| match &d.value {
            ApiIssueDetailValue::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }

    fn has_text(issue: &DerivedIssue, label: &str, value: &str) -> bool {
        detail_text(issue, label) == Some(value)
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
        assert!(has_text(&issues[0], "utilization", "88%"));
        assert!(has_text(&issues[0], "remaining", "14 W"));
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
        assert!(has_text(&issues[0], "threshold", "50%"));
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
                    root_port_state: Some("forwarding".to_string()),
                    path_cost: Some(20000),
                    reported: None,
                    root_mismatch: true,
                    root_mismatch_detail: Some(json::ApiStpRootMismatchDetail {
                        computed_root_fqdn: "core1.example.com".to_string(),
                        computed_root_hostname: "core1".to_string(),
                        computed_root_mac: Some("cc:98:91:6a:16:80".to_string()),
                        computed_root_priority: Some(28672),
                        reported_root_mac: Some("5c:e1:76:60:9b:00".to_string()),
                        reported_root_priority: Some(4096),
                        reported_root_monitored: false,
                        reported_root_superior: Some(true),
                    }),
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
                    root_mismatch_detail: None,
                    orphan: true,
                },
            ],
            blocked_links: vec![],
            flags: vec!["no-root".to_string(), "multiple-root-ports:core1.example.com".to_string()],
            multiple_roots_detail: None,
        };
        let issues = stp_tree_issues(&tree);
        let by_kind: HashMap<&str, &DerivedIssue> = issues.iter().map(|i| (i.kind.as_str(), i)).collect();
        assert_eq!(by_kind["stp-flag:no-root"].severity, SEV_BAD);
        assert_eq!(by_kind["stp-flag:no-root"].fqdn, ""); // vlan-level
        assert_eq!(by_kind["stp-flag:multiple-root-ports"].fqdn, "core1.example.com");
        // dist1's reported root is a *superior, unmonitored* bridge, so it is
        // the low-severity "unmonitored root", not a critical mismatch.
        assert_eq!(by_kind["stp-unmonitored-root"].fqdn, "dist1.example.com");
        assert_eq!(by_kind["stp-unmonitored-root"].severity, SEV_WARN);
        assert!(!by_kind.contains_key("stp-root-mismatch"));
        assert_eq!(by_kind["stp-orphan"].fqdn, "access1.example.com");
        assert_eq!(by_kind["stp-orphan"].severity, SEV_WARN);
        // Distinct subject keeps per-vlan flags unique.
        assert_eq!(by_kind["stp-flag:no-root"].issue_key(), "|stp-flag:no-root|vlan10");

        // The detail still carries both sides of the disagreement.
        let mismatch = by_kind["stp-unmonitored-root"];
        let reported = mismatch.detail.iter().find(|d| d.label == "reported root").unwrap();
        assert_eq!(
            reported.value,
            ApiIssueDetailValue::Mac { mac: "5c:e1:76:60:9b:00".to_string(), monitored: false },
        );
        let jaspy_root = mismatch.detail.iter().find(|d| d.label == "jaspy's elected root").unwrap();
        assert_eq!(
            jaspy_root.value,
            ApiIssueDetailValue::Device { fqdn: "core1.example.com".to_string(), hostname: "core1".to_string() },
        );
        // Superior + off-fleet → a warn verdict (unmonitored upstream, not split-brain).
        let verdict = mismatch.detail.iter().find(|d| d.label == "verdict").unwrap();
        match &verdict.value {
            ApiIssueDetailValue::Verdict { tone, text } => {
                assert_eq!(tone, "warn");
                assert!(text.contains("superior"));
            }
            other => panic!("expected a verdict, got {:?}", other),
        }
        // Every mismatch links to the VLAN's spanning tree.
        assert!(mismatch.detail.iter().any(|d| matches!(&d.value, ApiIssueDetailValue::Link { href, .. } if href == "/stp?vlan=10")));
    }

    // A minimal mismatched node carrying the given disagreement context.
    fn mismatch_node(superior: Option<bool>, monitored: bool) -> json::ApiStpNode {
        json::ApiStpNode {
            fqdn: "sw.example.com".to_string(),
            depth: 0,
            parent: None,
            parent_interface: None,
            root_port_interface_name: Some("Te1/1/1".to_string()),
            root_port_state: Some("forwarding".to_string()),
            path_cost: Some(20000),
            reported: None,
            root_mismatch: true,
            root_mismatch_detail: Some(json::ApiStpRootMismatchDetail {
                computed_root_fqdn: "core.example.com".to_string(),
                computed_root_hostname: "core".to_string(),
                computed_root_mac: Some("cc:98:91:6a:16:80".to_string()),
                computed_root_priority: Some(28672),
                reported_root_mac: Some("5c:e1:76:60:9b:00".to_string()),
                reported_root_priority: Some(4096),
                reported_root_monitored: monitored,
                reported_root_superior: superior,
            }),
            orphan: false,
        }
    }

    fn verdict_tone(issue: &DerivedIssue) -> Option<&str> {
        issue.detail.iter().find(|d| d.label == "verdict").and_then(|d| match &d.value {
            ApiIssueDetailValue::Verdict { tone, .. } => Some(tone.as_str()),
            _ => None,
        })
    }

    fn tree_with_node(node: json::ApiStpNode) -> json::ApiStpTree {
        json::ApiStpTree { vlan: 63, roots: vec!["core.example.com".to_string()], nodes: vec![node], blocked_links: vec![], flags: vec![], multiple_roots_detail: None }
    }

    #[test]
    fn human_secs_scales_by_magnitude() {
        assert_eq!(human_secs(-5), "0s"); // clamps negatives
        assert_eq!(human_secs(0), "0s");
        assert_eq!(human_secs(45), "45s");
        assert_eq!(human_secs(90), "1m");
        assert_eq!(human_secs(3600), "1h 0m");
        assert_eq!(human_secs(3660), "1h 1m");
        // ~154 days (the mobydick tele-sw1 topology-change age) → whole days.
        assert_eq!(human_secs(13_364_950), "154d");
    }

    #[test]
    fn root_mismatch_verdict_tone_by_scenario() {
        // Superior + off-fleet → warn (unmonitored upstream, the common case).
        let warn = stp_tree_issues(&tree_with_node(mismatch_node(Some(true), false)));
        assert_eq!(verdict_tone(&warn[0]), Some("warn"));

        // Superior + monitored → bad (real split-brain between two fleet bridges).
        let bad = stp_tree_issues(&tree_with_node(mismatch_node(Some(true), true)));
        assert_eq!(verdict_tone(&bad[0]), Some("bad"));

        // Reported root is weaker → neutral (stale/partitioned data on this node).
        let neutral = stp_tree_issues(&tree_with_node(mismatch_node(Some(false), false)));
        assert_eq!(verdict_tone(&neutral[0]), Some("neutral"));

        // Priority unknown → no verdict at all (never guess which is superior).
        let unknown = stp_tree_issues(&tree_with_node(mismatch_node(None, false)));
        assert_eq!(verdict_tone(&unknown[0]), None);
    }

    #[test]
    fn superior_unmonitored_root_is_warn_not_critical() {
        // The mobydick tele-sw1 case: reported root is superior AND off-fleet →
        // a warn "root not monitored", not a critical mismatch, and no blame on
        // the reporting device.
        let issues = stp_tree_issues(&tree_with_node(mismatch_node(Some(true), false)));
        assert_eq!(issues[0].kind, "stp-unmonitored-root");
        assert_eq!(issues[0].severity, SEV_WARN);
        assert!(issues[0].description.contains("not monitored"));
        assert!(!issues[0].description.contains("disagrees"));
    }

    #[test]
    fn monitored_or_weaker_disagreement_stays_critical_mismatch() {
        // A superior *monitored* root, or a weaker reported root, is a genuine
        // fault → the critical stp-root-mismatch is preserved.
        for (superior, monitored) in [(Some(true), true), (Some(false), false), (None, false)] {
            let issues = stp_tree_issues(&tree_with_node(mismatch_node(superior, monitored)));
            assert_eq!(issues[0].kind, "stp-root-mismatch", "superior={:?} monitored={}", superior, monitored);
            assert_eq!(issues[0].severity, SEV_BAD);
        }
    }

    fn multiple_roots_tree(adjacent: Option<bool>, a_has_vlan: bool, b_has_vlan: bool) -> json::ApiStpTree {
        json::ApiStpTree {
            vlan: 10,
            roots: vec!["a.example.com".to_string(), "b.example.com".to_string()],
            nodes: vec![],
            blocked_links: vec![],
            flags: vec!["multiple-roots".to_string()],
            multiple_roots_detail: Some(json::ApiStpMultipleRootsDetail {
                roots: vec![
                    json::ApiStpRootClaim { fqdn: "b.example.com".to_string(), hostname: "b".to_string(), mac: Some("00:00:00:00:00:0b".to_string()), priority: Some(24586), preferred: true },
                    json::ApiStpRootClaim { fqdn: "a.example.com".to_string(), hostname: "a".to_string(), mac: Some("00:00:00:00:00:0a".to_string()), priority: Some(28682), preferred: false },
                ],
                adjacent,
                connecting_link: match adjacent {
                    Some(true) => Some(json::ApiStpLinkEnds { a_fqdn: "a.example.com".to_string(), a_interface: "Te1/0/12".to_string(), a_has_vlan, b_fqdn: "b.example.com".to_string(), b_interface: "Te1/1/8".to_string(), b_has_vlan }),
                    _ => None,
                },
            }),
        }
    }

    #[test]
    fn multiple_roots_shared_vlan_path_is_critical_with_evidence() {
        // VLAN on both ends of the link → genuine convergence failure → bad.
        let issues = stp_tree_issues(&multiple_roots_tree(Some(true), true, true));
        let issue = &issues[0];
        assert_eq!(issue.kind, "stp-flag:multiple-roots");
        assert_eq!(issue.severity, SEV_BAD);
        // Both claimants listed, exactly one preferred, the lower bridge ID first.
        let claims: Vec<_> = issue.detail.iter().filter_map(|d| match &d.value {
            ApiIssueDetailValue::StpRoot { hostname, preferred, .. } => Some((hostname.as_str(), *preferred)),
            _ => None,
        }).collect();
        assert_eq!(claims.len(), 2);
        assert_eq!(claims.iter().filter(|(_, p)| *p).count(), 1);
        assert_eq!(claims[0], ("b", true));
        assert!(issue.detail.iter().any(|d| d.label == "roots connected"));
        assert_eq!(verdict_tone(issue), Some("bad"));
    }

    #[test]
    fn multiple_roots_asymmetric_trunk_is_warn_and_names_the_gap() {
        // VLAN on one end only → asymmetric trunk → warn, naming the port.
        let issues = stp_tree_issues(&multiple_roots_tree(Some(true), true, false));
        let issue = &issues[0];
        assert_eq!(issue.severity, SEV_WARN);
        assert_eq!(verdict_tone(issue), Some("warn"));
        let verdict = issue.detail.iter().find(|d| d.label == "verdict").unwrap();
        if let ApiIssueDetailValue::Verdict { text, .. } = &verdict.value {
            assert!(text.contains("asymmetric trunk"));
            assert!(text.contains("Te1/1/8")); // the end missing the VLAN (b)
        } else {
            panic!("expected a verdict");
        }
    }

    #[test]
    fn multiple_roots_disjoint_is_downgraded_to_warn() {
        let issues = stp_tree_issues(&multiple_roots_tree(Some(false), false, false));
        let issue = &issues[0];
        assert_eq!(issue.kind, "stp-flag:multiple-roots");
        assert_eq!(issue.severity, SEV_WARN); // no path between roots → likely separate segments
        assert_eq!(verdict_tone(issue), Some("neutral"));
    }

    fn pc_member(ifindex: i64, name: &str, up: Option<bool>, bundled: bool, peer_fqdn: Option<&str>) -> json::ApiPortChannelMember {
        json::ApiPortChannelMember {
            ifindex,
            name: Some(name.to_string()),
            up,
            connected_to: peer_fqdn.map(|f| json::ApiInterfaceConnection { fqdn: f.to_string(), interface: String::new() }),
            actor_state: vec![],
            partner_state: vec![],
            partner_port: None,
            bundled,
        }
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
        let issues = port_channel_issues("dist1.example.com", &pc, &HashMap::new());
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
    fn one_down_uplink_member_yields_a_focused_verdict() {
        // A 2-member LACP uplink to core1 with one member physically down: the
        // collector reports it as an unbundled member (empty actor state) whose
        // interface is oper-down.
        let pc = json::ApiPortChannel {
            ifindex: 5001,
            name: Some("Po1".to_string()),
            up: Some(true),
            protocol: "lacp".to_string(),
            partner_system_id: None,
            members: vec![
                pc_member(10101, "Te1/1/1", Some(true), true, Some("core1.example.com")),
                pc_member(10105, "Te1/1/5", Some(false), false, Some("core1.example.com")),
            ],
            // A down member still shows up as "not bundled" from the LACP walk.
            warnings: vec!["member-not-bundled:Te1/1/5".to_string()],
        };
        // dist2 is one hop below the root (core1); the guess should point here.
        let depth = HashMap::from([
            ("dist2.example.com".to_string(), 1),
            ("core1.example.com".to_string(), 0),
        ]);
        let issues = port_channel_issues("dist2.example.com", &pc, &depth);

        // Exactly one issue: the focused link-down verdict, NOT the raw
        // "not bundled" warning (which is suppressed for the down member).
        assert_eq!(issues.len(), 1);
        let issue = &issues[0];
        assert_eq!(issue.kind, "lag:member-link-down");
        assert_eq!(issue.severity, SEV_WARN); // degraded, not down → yellow
        assert_eq!(issue.subject, "5001:Te1/1/5");
        assert_eq!(verdict_tone(issue), Some("warn"));
        let verdict = issue.detail.iter().find_map(|d| match &d.value {
            ApiIssueDetailValue::Verdict { text, .. } => Some(text.clone()),
            _ => None,
        }).unwrap();
        assert!(verdict.contains("Te1/1/5"), "names the down member");
        assert!(verdict.contains("DOWN"));
        assert!(verdict.contains("cable"));
        assert!(verdict.contains("most likely here"), "dist2 is further from root: {}", verdict);
    }

    #[test]
    fn down_member_with_no_survivor_is_not_a_degraded_uplink() {
        // Every member down = the whole bundle is down; that is not the
        // "one uplink loose" case, so no link-down verdict fires (the generic
        // not-bundled warnings still describe it).
        let pc = json::ApiPortChannel {
            ifindex: 5001,
            name: Some("Po1".to_string()),
            up: Some(false),
            protocol: "lacp".to_string(),
            partner_system_id: None,
            members: vec![
                pc_member(10101, "Te1/1/1", Some(false), false, Some("core1.example.com")),
                pc_member(10105, "Te1/1/5", Some(false), false, Some("core1.example.com")),
            ],
            warnings: vec![
                "member-not-bundled:Te1/1/1".to_string(),
                "member-not-bundled:Te1/1/5".to_string(),
            ],
        };
        let issues = port_channel_issues("dist2.example.com", &pc, &HashMap::new());
        assert!(issues.iter().all(|i| i.kind != "lag:member-link-down"));
        assert_eq!(issues.iter().filter(|i| i.kind == "lag:member-not-bundled").count(), 2);
    }

    #[test]
    fn down_member_whose_only_survivor_is_unhealthy_is_not_a_degraded_uplink() {
        // a-02-style: one member's link is down while the only other member is
        // up but not bundling (misconfigured). That is not a clean "lost one of
        // several working uplinks" — the misconfig warnings own it, so no
        // link-down verdict fires.
        let pc = json::ApiPortChannel {
            ifindex: 5001,
            name: Some("Po1".to_string()),
            up: Some(true),
            protocol: "lacp".to_string(),
            partner_system_id: None,
            members: vec![
                pc_member(10101, "Te1/1/1", Some(false), false, Some("dist1.example.com")),
                pc_member(10102, "Te1/1/2", Some(true), false, Some("core1.example.com")),
            ],
            warnings: vec!["member-no-lacp-partner:Te1/1/2".to_string()],
        };
        let issues = port_channel_issues("access2.example.com", &pc, &HashMap::new());
        assert!(issues.iter().all(|i| i.kind != "lag:member-link-down"));
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
