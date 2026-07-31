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

// --- Issue type catalog & suppression ------------------------------------

// The persisted Setting key holding the JSON array of suppressed issue-type
// `kind`s. A suppressed kind is filtered out of every derivation (see
// collect_issues) so it never appears in the list, the tracker, or per-device
// views — distinct from acknowledging a single instance.
pub const SUPPRESSED_SETTING: &str = "suppressed_issue_types";

// One entry in the authoritative catalog of issue types the system can raise.
// `kind` matches DerivedIssue::kind exactly (the suppression granularity);
// title/description are type-level copy for the management UI.
#[derive(Clone, Debug, PartialEq)]
pub struct IssueTypeInfo {
    pub kind: &'static str,
    pub category: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

// Every issue `kind` the derivation can emit, grouped by category. Single
// source of truth for "what types exist"; keep in sync with the constructors
// in this module. Some kinds are compound (`stp-flag:*`, `lag:*`) — each full
// kind string is its own suppressible type.
pub fn issue_type_catalog() -> Vec<IssueTypeInfo> {
    vec![
        IssueTypeInfo { kind: "device-down", category: "device", title: "Device down", description: "A monitored device has stopped responding to polls." },
        IssueTypeInfo { kind: "poe-pse-down", category: "poe", title: "PoE power supply not operational", description: "A PSE power group is not on and cannot source PoE power." },
        IssueTypeInfo { kind: "poe-budget", category: "poe", title: "PoE budget near capacity", description: "A PSE group is running near (or over) its power budget." },
        IssueTypeInfo { kind: "iface-flapping", category: "interface", title: "Interface flapping", description: "An infrastructure port has gone up/down repeatedly in a short window." },
        IssueTypeInfo { kind: "iface-stale", category: "interface", title: "Interface not polling", description: "An infrastructure port has stopped answering polls." },
        IssueTypeInfo { kind: "iface-errors", category: "interface", title: "Interface errors", description: "An infrastructure port is accumulating input/output errors." },
        IssueTypeInfo { kind: "iface-discards", category: "interface", title: "Interface discards", description: "An infrastructure port is discarding packets." },
        IssueTypeInfo { kind: "iface-high-util", category: "interface", title: "High utilization", description: "An infrastructure port peaked at high link utilization." },
        IssueTypeInfo { kind: "iface-speed", category: "interface", title: "Speed renegotiation", description: "An infrastructure port renegotiated its link speed." },
        IssueTypeInfo { kind: "iface-err-disabled", category: "interface", title: "Port err-disabled", description: "The switch has error-disabled a port (e.g. BPDU guard, port-security, link-flap); it stays down until recovery." },
        IssueTypeInfo { kind: "stp-flag:no-root", category: "stp", title: "STP: no root bridge", description: "A VLAN has spanning-tree nodes but no elected root bridge." },
        IssueTypeInfo { kind: "stp-flag:multiple-roots", category: "stp", title: "STP: multiple roots", description: "A VLAN has more than one root bridge." },
        IssueTypeInfo { kind: "stp-flag:cycle", category: "stp", title: "STP: cycle", description: "A VLAN's spanning tree contains a cycle." },
        IssueTypeInfo { kind: "stp-flag:multiple-root-ports", category: "stp", title: "STP: multiple root ports", description: "A device has multiple root ports on one VLAN." },
        IssueTypeInfo { kind: "stp-unmonitored-root", category: "stp", title: "STP root not monitored", description: "A VLAN's root bridge is a superior bridge that jaspy does not monitor." },
        IssueTypeInfo { kind: "stp-root-mismatch", category: "stp", title: "STP root mismatch", description: "Devices disagree about a VLAN's root bridge." },
        IssueTypeInfo { kind: "stp-orphan", category: "stp", title: "STP orphan", description: "A device has a root port whose upstream neighbour is unresolved." },
        IssueTypeInfo { kind: "lag:member-link-down", category: "lag", title: "Port-channel: uplink member down", description: "A port-channel has a member link down while others still carry traffic." },
        IssueTypeInfo { kind: "lag:speed-mismatch", category: "lag", title: "Port-channel: member speed mismatch", description: "A port-channel's up members negotiated different link speeds — often a faulty cable forcing one leg down to a lower speed." },
        IssueTypeInfo { kind: "lag:not-lacp", category: "lag", title: "Port-channel: not running LACP", description: "A port-channel is negotiated with PAgP or static mode instead of LACP." },
        IssueTypeInfo { kind: "lag:single-member", category: "lag", title: "Port-channel: has only one member", description: "A port-channel has only one member (may be an intentional single uplink)." },
        IssueTypeInfo { kind: "lag:member-no-lacp-partner", category: "lag", title: "Port-channel: member has no LACP partner", description: "A port-channel member is not seeing an LACP partner." },
        IssueTypeInfo { kind: "lag:member-not-bundled", category: "lag", title: "Port-channel: member is not bundled", description: "A port-channel member is not bundled into the aggregate." },
        IssueTypeInfo { kind: "lag:members-report-different-partners", category: "lag", title: "Port-channel: members report different LACP partners", description: "A port-channel's members report different LACP partners." },
        IssueTypeInfo { kind: "lag:members-wired-to-different-devices", category: "lag", title: "Port-channel: members are wired to different devices", description: "A port-channel's members are wired to different neighbouring devices." },
        IssueTypeInfo { kind: "lag:far-end-lag-not-found", category: "lag", title: "Port-channel: far-end aggregate not found", description: "The far-end aggregate of a port-channel could not be found." },
        IssueTypeInfo { kind: "lag:far-end-member-count-mismatch", category: "lag", title: "Port-channel: far-end member count mismatch", description: "A port-channel and its far-end aggregate report different member counts." },
    ]
}

pub fn is_known_kind(kind: &str) -> bool {
    issue_type_catalog().iter().any(|t| t.kind == kind)
}

// Parse the persisted JSON array of suppressed kinds. Tolerant of a malformed
// value (returns empty) so a bad Setting can never break issue derivation.
pub fn parse_suppressed(json: &str) -> HashSet<String> {
    serde_json::from_str::<Vec<String>>(json)
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

// Serialize the suppressed set as a sorted JSON array (stable on disk).
pub fn serialize_suppressed(set: &HashSet<String>) -> String {
    let mut v: Vec<&String> = set.iter().collect();
    v.sort();
    serde_json::to_string(&v).unwrap_or_else(|_| "[]".to_string())
}

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

// A port-channel member row: interface name plus its physical facts (oper
// state, speed, optic/media) and the link's far end (resolved peer device or a
// raw CDP neighbor). Built from an ApiPortChannelMember.
fn member_row(k: &str, m: &json::ApiPortChannelMember) -> ApiIssueDetail {
    let name = m.name.clone().unwrap_or_else(|| format!("ifIndex {}", m.ifindex));
    let state = m.up.map(|up| if up { "up".to_string() } else { "down".to_string() });
    ApiIssueDetail {
        label: k.to_string(),
        value: ApiIssueDetailValue::Member {
            name,
            state,
            speed: m.speed,
            media: m.media.clone(),
            peer: m.connected_to.clone(),
            cdp: m.cdp_neighbor.clone(),
        },
    }
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
    // Set only when this issue is one end of a fault the /issues page should
    // combine with the far end into a single row (see LINK_GROUPABLE). Both
    // ends of the same link compute an identical key, so the frontend groups by
    // it. `None` for everything else — the row stays single. Additive: it does
    // not affect `issue_key`, so acking is still per-end.
    pub group_key: Option<String>,
}

impl DerivedIssue {
    pub fn issue_key(&self) -> String {
        format!("{}|{}|{}", self.fqdn, self.kind, self.subject)
    }
}

// Interface-health kinds that describe a property of an inter-switch *link*
// (rather than one port in isolation), so both ends independently detect them.
// These are the kinds the /issues page combines into a single two-ended row.
const LINK_GROUPABLE: &[&str] = &[
    "iface-high-util",
    "iface-flapping",
    "iface-errors",
    "iface-discards",
    "iface-speed",
];

// Deterministic key shared by both ends of one link's fault: the kind plus the
// two endpoints (each "<fqdn>|<iface>") sorted, so it is identical regardless
// of which end computes it. Only meaningful when the far end is a monitored
// peer (both ends are jaspy-managed and each raises its own issue).
fn link_group_key(kind: &str, a_fqdn: &str, a_iface: &str, b_fqdn: &str, b_iface: &str) -> String {
    let mut ends = [format!("{}|{}", a_fqdn, a_iface), format!("{}|{}", b_fqdn, b_iface)];
    ends.sort();
    format!("{}|{}<->{}", kind, ends[0], ends[1])
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
        group_key: None,
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
                group_key: None,
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
            group_key: None,
        });
    }
    out
}

// --- Interface health -----------------------------------------------------

// Splits a tripped interface-health summary into one issue per active signal
// (so each can be acknowledged independently). Nothing is emitted when the
// summary's severity is None (no signal crossed its configured threshold).
//
// `is_infrastructure_port` gates the whole thing: an access/edge port (no
// port-channel membership and no discovered CDP/LLDP neighbour) is silenced
// entirely — its flaps/errors/discards are end-host noise, not a fleet problem,
// and they otherwise drown out the issues that matter. The caller decides the
// flag; see collect_issues.
//
// `connected_to`/`cdp_neighbor` are the interface's resolved far end (a
// monitored peer, else a raw CDP/LLDP neighbour); the flapping issue uses them
// to name the other side of the link so an operator can see it's a
// switch-to-switch link and jump to the far-end device.
pub fn interface_health_issues(
    fqdn: &str,
    ifindex: i32,
    iface_name: &str,
    h: &json::ApiInterfaceHealth,
    is_infrastructure_port: bool,
    connected_to: Option<&json::ApiInterfaceConnection>,
    cdp_neighbor: Option<&json::ApiCdpNeighbor>,
) -> Vec<DerivedIssue> {
    if !is_infrastructure_port || h.severity.is_none() {
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
        // Combine with the far end only when this is a link-level fault AND the
        // far end is a monitored peer (which raises its own matching issue). An
        // unmonitored CDP/LLDP neighbour has no issue to pair with, so it stays
        // a single-ended row.
        group_key: connected_to.filter(|_| LINK_GROUPABLE.contains(&kind)).map(|peer| {
            link_group_key(kind, fqdn, iface_name, &peer.fqdn, &peer.interface)
        }),
    };

    let mut out = Vec::new();
    // Only a link that has recovered repeatedly (flap_count crossed the
    // threshold server-side) is flapping. A single recovery — a first cable
    // plug-in, a one-off reboot — sets flap_count == 1 but leaves `flapping`
    // false, so it shows in the detail metrics without raising this alert.
    if h.flapping {
        let mut detail = vec![
            pair("recoveries", h.flap_count.to_string()),
            pair("last came up", h.last_flap_secs_ago.map(|s| format!("{}s ago", s)).unwrap_or_else(|| "—".to_string())),
            pair("window", format!("{}s", h.flap_window_secs)),
        ];
        // The far end, so the operator can see whether this is a switch-to-switch
        // link (and jump to the other device). A monitored peer becomes a link;
        // an unmonitored CDP/LLDP neighbour is shown as plain text.
        if let Some(peer) = connected_to {
            detail.push(verdict_row(
                "verdict",
                format!(
                    "This is an inter-switch link to {} ({}). A flapping link between two switches is usually a bad cable, a dirty/failing SFP, or a duplex/speed mismatch — check both ends.",
                    hostname_of(&peer.fqdn), peer.interface
                ),
                "bad",
            ));
            detail.push(device_row("connected to", peer.fqdn.clone()));
            detail.push(iface_row("far-end port", peer.interface.clone(), None));
        } else if let Some(cdp) = cdp_neighbor {
            let neighbor = match &cdp.device_port {
                Some(port) if !port.trim().is_empty() => format!("{} ({})", cdp.device_id, port),
                _ => cdp.device_id.clone(),
            };
            detail.push(pair("neighbor (CDP/LLDP)", neighbor));
        }
        out.push(mk(
            "iface-flapping",
            SEV_BAD,
            "Interface flapping",
            format!("{} {} bounced down/up {} times in the last {}s", host, iface_name, h.flap_count, h.flap_window_secs),
            detail,
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

// A friendlier one-line explanation for the common err-disable causes; empty
// for the long tail (the raw cause name still shows in its own row).
fn err_disable_cause_hint(cause: &str) -> &'static str {
    match cause {
        "bpduGuard" => "BPDU guard tripped — a BPDU arrived on a PortFast/edge port (something running spanning tree, e.g. a switch, was plugged into an access port).",
        "portSecurityViolation" => "Port-security violation — a disallowed MAC, or more MACs than permitted, were seen on the port.",
        "linkFlap" => "Link-flap protection — the port bounced up/down too many times in a short window.",
        "dhcpRateLimit" => "DHCP snooping rate-limit exceeded.",
        "arpInspection" => "Dynamic ARP inspection rate-limit exceeded.",
        "stormControl" => "Storm control threshold exceeded (broadcast/multicast/unknown-unicast flood).",
        "loopDetect" | "portLoopback" => "A loop was detected on the port.",
        "udld" | "udldUniDir" | "udldTxRxLoop" | "udldNeighbourMismatch" | "udldEmptyEcho" | "udldAggrasiveModeLinkFailed" => "UDLD detected a unidirectional link or miswired fiber.",
        _ => "",
    }
}

// A port the switch has error-disabled (CISCO-ERR-DISABLE-MIB). Unlike the
// interface-health signals this fires on ANY port — err-disable on an
// access/edge port (BPDU guard, port-security) is a genuine, actionable fault,
// not end-host noise. `cause` is the MIB enum name (e.g. "bpduGuard"); the
// far end is attached when it is a monitored peer. Single-ended (group_key
// None): err-disable is a local action, not a shared link property.
pub fn err_disabled_issue(
    fqdn: &str,
    ifindex: i32,
    iface_name: &str,
    cause: Option<&str>,
    recover_secs: Option<i32>,
    connected_to: Option<&json::ApiInterfaceConnection>,
) -> DerivedIssue {
    let host = hostname_of(fqdn);
    let cause_raw = cause.unwrap_or("unknown");
    let hint = err_disable_cause_hint(cause_raw);
    let verdict = if hint.is_empty() {
        format!("{} was error-disabled by the switch (cause: {}). The port is forced down until it recovers — investigate the cause, then clear err-disable (shut/no shut) or wait for auto-recovery.", iface_name, cause_raw)
    } else {
        format!("{} was error-disabled by the switch. {} The port is forced down until it recovers — fix the cause, then clear err-disable (shut/no shut) or wait for auto-recovery.", iface_name, hint)
    };
    let mut detail = vec![
        verdict_row("verdict", verdict, "bad"),
        pair("cause", cause_raw.to_string()),
    ];
    match recover_secs {
        Some(secs) if secs > 0 => detail.push(pair("auto-recovers in", format!("{}s", secs))),
        _ => detail.push(pair("auto-recovery", "not scheduled — manual clear required")),
    }
    if let Some(peer) = connected_to {
        detail.push(device_row("connected to", peer.fqdn.clone()));
        detail.push(iface_row("far-end port", peer.interface.clone(), None));
    }
    DerivedIssue {
        fqdn: fqdn.to_string(),
        hostname: host.clone(),
        kind: "iface-err-disabled".to_string(),
        subject: ifindex.to_string(),
        severity: SEV_BAD.to_string(),
        title: "Port err-disabled".to_string(),
        description: format!("{} {} is err-disabled ({})", host, iface_name, cause_raw),
        subject_label: Some(iface_name.to_string()),
        detail,
        group_key: None,
    }
}

// --- STP ------------------------------------------------------------------

// Structural spanning-tree anomalies for one VLAN's computed tree. Routine
// blocked (alternate/backup) ports are deliberately NOT issues — blocking is
// STP working correctly; only genuine anomalies are surfaced.
pub fn stp_tree_issues(tree: &json::ApiStpTree, expected_roots: &HashMap<String, Option<String>>) -> Vec<DerivedIssue> {
    let vlan = tree.vlan;
    // Append the VLAN name to the compact labels when the switches agree on a
    // single name; stay id-only when unnamed or when they disagree (ambiguous).
    let name_suffix = match tree.names.as_slice() {
        [name] => format!(" ({})", name),
        _ => String::new(),
    };
    let vlan_label = format!("VLAN {}{}", vlan, name_suffix);
    let subject = format!("vlan{}", vlan);
    let mut out = Vec::new();
    let stp_tree_link = link_row("spanning tree", format!("VLAN {}{} tree", vlan, name_suffix), format!("/stp?vlan={}", vlan));

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
                // Subtract roots the operator has marked as expected/known
                // separate trees. Only alert when more than one *unacknowledged*
                // root remains — acknowledging the leftover leaves a single
                // legitimate root, while a genuinely new/unexpected root keeps
                // the count above one and re-alerts on its own.
                let mut annotated = d.clone();
                let unacked = crate::utilities::stp::apply_expected_roots(&mut annotated, expected_roots);
                if unacked <= 1 {
                    continue;
                }
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
                multiple_roots_detail(&annotated, &stp_tree_link, vlan)
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
            group_key: None,
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
                group_key: None,
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
                group_key: None,
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
            group_key: None,
        });
    }

    // Speed mismatch: two or more members that are up but negotiated to
    // different link speeds. The classic cause is a faulty cable (or a
    // duplex/auto-negotiation fault) forcing one leg of an N×1G bundle down to
    // 100M — the bundle still forms, but throughput is capped and traffic
    // hashes onto legs of unequal capacity. Reported as an error because it is
    // a silent physical fault that degrades a link people believe is at full
    // speed. Only up members with a known positive speed are compared.
    let member_speeds: Vec<(String, i64)> = pc
        .members
        .iter()
        .filter(|m| m.up == Some(true))
        .filter_map(|m| {
            m.speed
                .filter(|s| *s > 0)
                .map(|s| (m.name.clone().unwrap_or_else(|| m.ifindex.to_string()), s))
        })
        .collect();
    if member_speeds.len() >= 2 {
        let distinct: HashSet<i64> = member_speeds.iter().map(|(_, s)| *s).collect();
        if distinct.len() >= 2 {
            let min = distinct.iter().min().copied().unwrap_or(0);
            let max = distinct.iter().max().copied().unwrap_or(0);
            let mut detail = vec![
                verdict_row(
                    "verdict",
                    format!(
                        "{} bundles members running at different link speeds ({} Mb/s vs {} Mb/s). A port-channel should aggregate identical-speed links; a member negotiated below the others is almost always a faulty cable or a duplex/auto-negotiation fault. The slow leg caps throughput and traffic hashes unevenly — check the {} Mb/s member's cabling and port settings.",
                        agg_name, max, min, min
                    ),
                    "bad",
                ),
                pair("port-channel", agg_name.clone()),
                pair("protocol", pc.protocol.clone()),
            ];
            for m in pc.members.iter() {
                detail.push(member_row("member", m));
            }
            issues.push(DerivedIssue {
                fqdn: fqdn.to_string(),
                hostname: host.clone(),
                kind: "lag:speed-mismatch".to_string(),
                // One per aggregate; the ifindex keeps the key stable.
                subject: format!("{}:speed", pc.ifindex),
                severity: SEV_BAD.to_string(),
                title: "Port-channel: member speed mismatch".to_string(),
                description: format!("{} {} has members at different link speeds ({}–{} Mb/s)", host, agg_name, min, max),
                subject_label: Some(agg_name.clone()),
                detail,
                group_key: None,
            });
        }
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
        ];
        if code == "single-member" {
            // A one-member port-channel is often a deliberate single SFP+
            // uplink; jaspy cannot see the intended member count, so this is a
            // heads-up rather than a definite fault. Enumerate the member(s)
            // with their physical facts (speed, optic, link status, neighbor)
            // so an operator can judge at a glance whether a link is missing.
            detail.push(verdict_row(
                "verdict",
                "only one member in this port-channel — normal for a single SFP+ uplink, but a missing second member would look the same. Check the member(s) below.",
                "neutral",
            ));
            for m in pc.members.iter() {
                detail.push(member_row("member", m));
            }
        } else {
            detail.push(pair("warning", warning.clone()));
            if let Some(s) = suffix.as_ref() {
                detail.push(pair("detail", s.clone()));
            }
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
            group_key: None,
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

    #[test]
    fn catalog_is_non_empty_with_unique_kinds() {
        let catalog = issue_type_catalog();
        assert!(!catalog.is_empty());
        let mut kinds: Vec<&str> = catalog.iter().map(|t| t.kind).collect();
        let count = kinds.len();
        kinds.sort();
        kinds.dedup();
        assert_eq!(kinds.len(), count, "catalog kinds must be unique");
        // Every catalog entry must classify into a known category.
        for t in &catalog {
            assert!(
                matches!(t.category, "device" | "poe" | "interface" | "stp" | "lag"),
                "unexpected category {} for {}", t.category, t.kind
            );
        }
        assert!(is_known_kind("lag:not-lacp"));
        assert!(!is_known_kind("lag:totally-made-up"));
    }

    #[test]
    fn suppressed_set_roundtrips_through_json() {
        let mut set = HashSet::new();
        set.insert("lag:not-lacp".to_string());
        set.insert("device-down".to_string());
        let json = serialize_suppressed(&set);
        // Sorted, stable on disk.
        assert_eq!(json, r#"["device-down","lag:not-lacp"]"#);
        assert_eq!(parse_suppressed(&json), set);
        // Tolerates garbage and empties.
        assert!(parse_suppressed("not json").is_empty());
        assert!(parse_suppressed("").is_empty());
        assert!(parse_suppressed("[]").is_empty());
    }

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
            flapping: false,
            last_flap_secs_ago: None,
            in_errors: 0,
            out_errors: 0,
            discards: 0,
            discards_high: false,
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
        assert!(interface_health_issues("sw1.example.com", 1, "Gi1/0/1", &health(None), true, None, None).is_empty());
    }

    // A fully tripped health summary used by several tests below.
    fn all_signals_tripped() -> json::ApiInterfaceHealth {
        let mut h = health(Some("bad"));
        h.flap_count = 3;
        h.flapping = true;
        h.last_flap_secs_ago = Some(12);
        h.in_errors = 5;
        h.discards = 7;
        h.discards_high = true;
        h.high_utilization = true;
        h.peak_utilization_pct = Some(95.0);
        h.speed_change_count = 1;
        h.last_speed_change = Some((Some(1000), 100));
        h
    }

    #[test]
    fn interface_signals_split_into_one_issue_each() {
        let h = all_signals_tripped();
        let issues = interface_health_issues("sw1.example.com", 10001, "Gi1/0/1", &h, true, None, None);
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
    fn access_port_health_is_suppressed() {
        // Same tripped signals, but a non-infrastructure port (no LAG, no
        // CDP/LLDP neighbour): every signal is silenced as end-host noise.
        let h = all_signals_tripped();
        assert!(interface_health_issues("sw1.example.com", 10001, "Gi1/0/1", &h, false, None, None).is_empty());
    }

    #[test]
    fn stale_interface_is_bad() {
        let mut h = health(Some("bad"));
        h.stale = true;
        let issues = interface_health_issues("sw1.example.com", 1, "Gi1", &h, true, None, None);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].kind, "iface-stale");
        assert_eq!(issues[0].severity, SEV_BAD);
    }

    #[test]
    fn flapping_issue_names_the_far_end() {
        let mut h = health(Some("bad"));
        h.flap_count = 4;
        h.flapping = true;

        // A monitored peer → a Device row (renders as a link) plus the far-end
        // port, so the operator can jump to the other switch.
        let peer = json::ApiInterfaceConnection { fqdn: "dist1.example.com".to_string(), interface: "Te1/1/3".to_string() };
        let issues = interface_health_issues("a02.example.com", 10101, "Te1/1/1", &h, true, Some(&peer), None);
        let flap = issues.iter().find(|i| i.kind == "iface-flapping").unwrap();
        let far = flap.detail.iter().find_map(|d| match &d.value {
            ApiIssueDetailValue::Device { fqdn, .. } => Some(fqdn.as_str()),
            _ => None,
        });
        assert_eq!(far, Some("dist1.example.com"));
        assert!(flap.detail.iter().any(|d| d.label == "far-end port"));

        // An unmonitored CDP/LLDP neighbour → plain text, no Device link.
        let cdp = json::ApiCdpNeighbor { device_id: "core-sw9".to_string(), device_port: Some("Gi0/1".to_string()) };
        let issues = interface_health_issues("a02.example.com", 10101, "Te1/1/1", &h, true, None, Some(&cdp));
        let flap = issues.iter().find(|i| i.kind == "iface-flapping").unwrap();
        assert!(!flap.detail.iter().any(|d| matches!(d.value, ApiIssueDetailValue::Device { .. })));
        assert_eq!(
            detail_text(flap, "neighbor (CDP/LLDP)"),
            Some("core-sw9 (Gi0/1)")
        );
    }

    #[test]
    fn link_groupable_issues_share_a_group_key() {
        // High utilization is a property of the link, so both ends detect it and
        // each raises its own issue. The two must compute an identical group_key
        // (so the /issues page combines them) while keeping distinct issue keys
        // (so acking is still per-end).
        let mut h = health(Some("warn"));
        h.high_utilization = true;
        h.peak_utilization_pct = Some(92.0);

        let a_to_b = json::ApiInterfaceConnection { fqdn: "b.example.com".to_string(), interface: "Gi1/0/24".to_string() };
        let side_a = interface_health_issues("a.example.com", 10001, "Gi1/0/1", &h, true, Some(&a_to_b), None);
        let util_a = side_a.iter().find(|i| i.kind == "iface-high-util").unwrap();

        // The far end sees the mirror image (its own port, peer = us).
        let b_to_a = json::ApiInterfaceConnection { fqdn: "a.example.com".to_string(), interface: "Gi1/0/1".to_string() };
        let side_b = interface_health_issues("b.example.com", 20002, "Gi1/0/24", &h, true, Some(&b_to_a), None);
        let util_b = side_b.iter().find(|i| i.kind == "iface-high-util").unwrap();

        assert!(util_a.group_key.is_some(), "a link-level issue with a monitored peer must be groupable");
        assert_eq!(util_a.group_key, util_b.group_key, "both ends of a link must share a group key");
        assert_ne!(util_a.issue_key(), util_b.issue_key(), "each end keeps its own ack key");
    }

    #[test]
    fn unmonitored_far_end_is_not_grouped() {
        // A CDP/LLDP neighbour jaspy does not poll raises no matching issue on
        // the far end, so there is nothing to combine with — stays single-ended.
        let mut h = health(Some("warn"));
        h.high_utilization = true;
        let cdp = json::ApiCdpNeighbor { device_id: "unmonitored-sw".to_string(), device_port: Some("Gi0/1".to_string()) };
        let issues = interface_health_issues("a.example.com", 10001, "Gi1/0/1", &h, true, None, Some(&cdp));
        let util = issues.iter().find(|i| i.kind == "iface-high-util").unwrap();
        assert!(util.group_key.is_none());
    }

    #[test]
    fn err_disabled_issue_is_bad_with_cause_and_far_end() {
        let peer = json::ApiInterfaceConnection { fqdn: "dist1.example.com".to_string(), interface: "Gi1/0/2".to_string() };
        let issue = err_disabled_issue("a01.example.com", 9, "Gi1/0/1", Some("bpduGuard"), Some(39), Some(&peer));
        assert_eq!(issue.kind, "iface-err-disabled");
        assert_eq!(issue.severity, SEV_BAD);
        assert_eq!(issue.subject, "9");
        assert_eq!(issue.issue_key(), "a01.example.com|iface-err-disabled|9");
        assert!(issue.group_key.is_none());
        assert!(has_text(&issue, "cause", "bpduGuard"));
        assert!(has_text(&issue, "auto-recovers in", "39s"));
        // The far end is attached as a device link + far-end port.
        assert!(issue.detail.iter().any(|d| matches!(&d.value, ApiIssueDetailValue::Device { fqdn, .. } if fqdn == "dist1.example.com")));
        // The bpduGuard verdict carries the friendly hint.
        let verdict = issue.detail.iter().find(|d| d.label == "verdict").unwrap();
        match &verdict.value {
            ApiIssueDetailValue::Verdict { tone, text } => {
                assert_eq!(tone, "bad");
                assert!(text.contains("BPDU guard"));
            }
            other => panic!("expected a verdict, got {:?}", other),
        }
    }

    #[test]
    fn err_disabled_issue_without_recovery_or_far_end() {
        let issue = err_disabled_issue("a01.example.com", 9, "Gi1/0/1", None, None, None);
        assert!(has_text(&issue, "cause", "unknown"));
        assert!(has_text(&issue, "auto-recovery", "not scheduled — manual clear required"));
        assert!(!issue.detail.iter().any(|d| matches!(d.value, ApiIssueDetailValue::Device { .. })));
    }

    #[test]
    fn non_link_kinds_are_never_grouped() {
        // "not polling" is a per-port/device condition, not a link property, so
        // it is never combined even with a monitored peer.
        let mut h = health(Some("bad"));
        h.stale = true;
        let peer = json::ApiInterfaceConnection { fqdn: "b.example.com".to_string(), interface: "Gi1/0/24".to_string() };
        let issues = interface_health_issues("a.example.com", 10001, "Gi1/0/1", &h, true, Some(&peer), None);
        let stale = issues.iter().find(|i| i.kind == "iface-stale").unwrap();
        assert!(stale.group_key.is_none());
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
            names: vec![],
            multiple_roots_detail: None,
        };
        let issues = stp_tree_issues(&tree, &HashMap::new());
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
        json::ApiStpTree { vlan: 63, roots: vec!["core.example.com".to_string()], nodes: vec![node], blocked_links: vec![], flags: vec![], names: vec![], multiple_roots_detail: None }
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
        let warn = stp_tree_issues(&tree_with_node(mismatch_node(Some(true), false)), &HashMap::new());
        assert_eq!(verdict_tone(&warn[0]), Some("warn"));

        // Superior + monitored → bad (real split-brain between two fleet bridges).
        let bad = stp_tree_issues(&tree_with_node(mismatch_node(Some(true), true)), &HashMap::new());
        assert_eq!(verdict_tone(&bad[0]), Some("bad"));

        // Reported root is weaker → neutral (stale/partitioned data on this node).
        let neutral = stp_tree_issues(&tree_with_node(mismatch_node(Some(false), false)), &HashMap::new());
        assert_eq!(verdict_tone(&neutral[0]), Some("neutral"));

        // Priority unknown → no verdict at all (never guess which is superior).
        let unknown = stp_tree_issues(&tree_with_node(mismatch_node(None, false)), &HashMap::new());
        assert_eq!(verdict_tone(&unknown[0]), None);
    }

    #[test]
    fn superior_unmonitored_root_is_warn_not_critical() {
        // The mobydick tele-sw1 case: reported root is superior AND off-fleet →
        // a warn "root not monitored", not a critical mismatch, and no blame on
        // the reporting device.
        let issues = stp_tree_issues(&tree_with_node(mismatch_node(Some(true), false)), &HashMap::new());
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
            let issues = stp_tree_issues(&tree_with_node(mismatch_node(superior, monitored)), &HashMap::new());
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
            names: vec![],
            multiple_roots_detail: Some(json::ApiStpMultipleRootsDetail {
                roots: vec![
                    json::ApiStpRootClaim { fqdn: "b.example.com".to_string(), hostname: "b".to_string(), mac: Some("00:00:00:00:00:0b".to_string()), priority: Some(24586), preferred: true, expected: false, note: None },
                    json::ApiStpRootClaim { fqdn: "a.example.com".to_string(), hostname: "a".to_string(), mac: Some("00:00:00:00:00:0a".to_string()), priority: Some(28682), preferred: false, expected: false, note: None },
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
    fn vlan_name_enriches_subject_label_only_when_agreed() {
        // A single agreed name is appended to the "Affected" label and the tree link.
        let mut tree = multiple_roots_tree(Some(true), true, true);
        tree.names = vec!["esports".to_string()];
        let issue = &stp_tree_issues(&tree, &HashMap::new())[0];
        assert_eq!(issue.subject_label.as_deref(), Some("VLAN 10 (esports)"));
        assert!(issue.detail.iter().any(|d| matches!(&d.value,
            ApiIssueDetailValue::Link { text, .. } if text == "VLAN 10 (esports) tree")));

        // No name → id only.
        let mut bare = multiple_roots_tree(Some(true), true, true);
        bare.names = vec![];
        assert_eq!(stp_tree_issues(&bare, &HashMap::new())[0].subject_label.as_deref(), Some("VLAN 10"));

        // Disagreement (>1 name) → id only, never an ambiguous label.
        let mut conflict = multiple_roots_tree(Some(true), true, true);
        conflict.names = vec!["esports".to_string(), "gaming".to_string()];
        assert_eq!(stp_tree_issues(&conflict, &HashMap::new())[0].subject_label.as_deref(), Some("VLAN 10"));
    }

    #[test]
    fn multiple_roots_shared_vlan_path_is_critical_with_evidence() {
        // VLAN on both ends of the link → genuine convergence failure → bad.
        let issues = stp_tree_issues(&multiple_roots_tree(Some(true), true, true), &HashMap::new());
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
        let issues = stp_tree_issues(&multiple_roots_tree(Some(true), true, false), &HashMap::new());
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
        let issues = stp_tree_issues(&multiple_roots_tree(Some(false), false, false), &HashMap::new());
        let issue = &issues[0];
        assert_eq!(issue.kind, "stp-flag:multiple-roots");
        assert_eq!(issue.severity, SEV_WARN); // no path between roots → likely separate segments
        assert_eq!(verdict_tone(issue), Some("neutral"));
    }

    fn expected_map(entries: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        entries.iter().map(|(f, n)| (f.to_string(), n.map(String::from))).collect()
    }

    fn three_roots_tree() -> json::ApiStpTree {
        json::ApiStpTree {
            vlan: 503,
            roots: vec!["a.example.com".to_string(), "b.example.com".to_string(), "c.example.com".to_string()],
            nodes: vec![],
            blocked_links: vec![],
            flags: vec!["multiple-roots".to_string()],
            names: vec![],
            multiple_roots_detail: Some(json::ApiStpMultipleRootsDetail {
                roots: vec![
                    json::ApiStpRootClaim { fqdn: "a.example.com".to_string(), hostname: "a".to_string(), mac: Some("00:00:00:00:00:0a".to_string()), priority: Some(24586), preferred: true, expected: false, note: None },
                    json::ApiStpRootClaim { fqdn: "b.example.com".to_string(), hostname: "b".to_string(), mac: Some("00:00:00:00:00:0b".to_string()), priority: Some(28682), preferred: false, expected: false, note: None },
                    json::ApiStpRootClaim { fqdn: "c.example.com".to_string(), hostname: "c".to_string(), mac: Some("00:00:00:00:00:0c".to_string()), priority: Some(32778), preferred: false, expected: false, note: None },
                ],
                adjacent: Some(false),
                connecting_link: None,
            }),
        }
    }

    fn has_multiple_roots(issues: &[DerivedIssue]) -> bool {
        issues.iter().any(|i| i.kind == "stp-flag:multiple-roots")
    }

    #[test]
    fn multiple_roots_with_one_expected_root_is_suppressed() {
        // Two roots, one acknowledged as a known leftover → one unacknowledged
        // root remains → the "multiple roots" issue is not emitted.
        let expected = expected_map(&[("a.example.com", Some("leftover, not in prod"))]);
        let issues = stp_tree_issues(&multiple_roots_tree(Some(true), true, true), &expected);
        assert!(!has_multiple_roots(&issues));
    }

    #[test]
    fn multiple_roots_all_expected_is_suppressed() {
        let expected = expected_map(&[("a.example.com", None), ("b.example.com", None)]);
        let issues = stp_tree_issues(&multiple_roots_tree(Some(true), true, true), &expected);
        assert!(!has_multiple_roots(&issues));
    }

    #[test]
    fn multiple_roots_expected_fqdn_not_a_root_has_no_effect() {
        // An acknowledgement whose fqdn is not currently a root does nothing:
        // both real roots stay unacknowledged and the issue still fires.
        let expected = expected_map(&[("ghost.example.com", Some("stale"))]);
        let issues = stp_tree_issues(&multiple_roots_tree(Some(true), true, true), &expected);
        assert!(has_multiple_roots(&issues));
    }

    #[test]
    fn three_roots_one_expected_still_alerts() {
        // Acknowledging one of three roots still leaves two unacknowledged → the
        // issue re-alerts, so a genuinely new/unexpected root is never masked.
        let expected = expected_map(&[("a.example.com", Some("known"))]);
        let issues = stp_tree_issues(&three_roots_tree(), &expected);
        assert!(has_multiple_roots(&issues));
        // But dropping to a single unacknowledged root suppresses it.
        let expected2 = expected_map(&[("a.example.com", None), ("b.example.com", None)]);
        let issues2 = stp_tree_issues(&three_roots_tree(), &expected2);
        assert!(!has_multiple_roots(&issues2));
    }

    fn pc_member(ifindex: i64, name: &str, up: Option<bool>, bundled: bool, peer_fqdn: Option<&str>) -> json::ApiPortChannelMember {
        json::ApiPortChannelMember {
            ifindex,
            name: Some(name.to_string()),
            up,
            speed: None,
            media: None,
            connected_to: peer_fqdn.map(|f| json::ApiInterfaceConnection { fqdn: f.to_string(), interface: String::new() }),
            cdp_neighbor: None,
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
            alias: None,
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
    fn port_channel_members_at_different_speeds_is_an_error() {
        let mk = |ifindex: i64, name: &str, up: Option<bool>, speed: Option<i64>| {
            let mut m = pc_member(ifindex, name, up, true, None);
            m.speed = speed;
            m
        };
        let pc = |members: Vec<json::ApiPortChannelMember>| json::ApiPortChannel {
            ifindex: 5001,
            name: Some("Po1".to_string()),
            alias: None,
            up: Some(true),
            protocol: "lacp".to_string(),
            partner_system_id: None,
            members,
            warnings: vec![],
        };

        // Two up members at different speeds (the faulty-cable scenario) → one
        // error, keyed per aggregate, naming both speeds.
        let issues = port_channel_issues(
            "sw1.example.com",
            &pc(vec![mk(10101, "Te1/0/1", Some(true), Some(1000)), mk(10105, "Te1/0/5", Some(true), Some(100))]),
            &HashMap::new(),
        );
        let mismatch = issues.iter().find(|i| i.kind == "lag:speed-mismatch").expect("a speed-mismatch issue");
        assert_eq!(mismatch.severity, SEV_BAD);
        assert_eq!(mismatch.subject, "5001:speed");
        assert_eq!(mismatch.issue_key(), "sw1.example.com|lag:speed-mismatch|5001:speed");
        assert!(mismatch.description.contains("100") && mismatch.description.contains("1000"));
        assert_eq!(verdict_tone(mismatch), Some("bad"));

        // Identical speeds → no issue.
        let issues = port_channel_issues(
            "sw1.example.com",
            &pc(vec![mk(10101, "Te1/0/1", Some(true), Some(1000)), mk(10105, "Te1/0/5", Some(true), Some(1000))]),
            &HashMap::new(),
        );
        assert!(!issues.iter().any(|i| i.kind == "lag:speed-mismatch"));

        // Only up members with a known speed are compared: a down/speed-unknown
        // member leaves a single comparable member, so no mismatch is raised.
        let issues = port_channel_issues(
            "sw1.example.com",
            &pc(vec![mk(10101, "Te1/0/1", Some(true), Some(1000)), mk(10105, "Te1/0/5", Some(false), None)]),
            &HashMap::new(),
        );
        assert!(!issues.iter().any(|i| i.kind == "lag:speed-mismatch"));
    }

    #[test]
    fn single_member_issue_lists_member_physical_facts() {
        // The single-member detail should enumerate each member with its speed,
        // optic, oper status and resolved neighbor — not just the bare code.
        let member = json::ApiPortChannelMember {
            ifindex: 10107,
            name: Some("Gi0/7".to_string()),
            up: Some(true),
            speed: Some(10000),
            media: Some("sfp: SFP-10GBase-LR".to_string()),
            connected_to: Some(json::ApiInterfaceConnection {
                fqdn: "gw1.example.com".to_string(),
                interface: "Te1/0/1".to_string(),
            }),
            cdp_neighbor: None,
            actor_state: vec!["synchronization".to_string(), "collecting".to_string(), "distributing".to_string()],
            partner_state: vec![],
            partner_port: None,
            bundled: true,
        };
        let pc = json::ApiPortChannel {
            ifindex: 5001,
            name: Some("Po1".to_string()),
            alias: None,
            up: Some(true),
            protocol: "lacp".to_string(),
            partner_system_id: None,
            members: vec![member],
            warnings: vec!["single-member".to_string()],
        };
        let issues = port_channel_issues("sw1.example.com", &pc, &HashMap::new());
        let single = issues.iter().find(|i| i.kind == "lag:single-member").expect("a single-member issue");
        let (name, state, speed, media, peer) = single
            .detail
            .iter()
            .find_map(|d| match &d.value {
                ApiIssueDetailValue::Member { name, state, speed, media, peer, .. } => {
                    Some((name.clone(), state.clone(), *speed, media.clone(), peer.clone()))
                }
                _ => None,
            })
            .expect("a member detail row");
        assert_eq!(name, "Gi0/7");
        assert_eq!(state.as_deref(), Some("up"));
        assert_eq!(speed, Some(10000));
        assert_eq!(media.as_deref(), Some("sfp: SFP-10GBase-LR"));
        assert_eq!(peer.map(|p| p.interface), Some("Te1/0/1".to_string()));
        // A neutral heads-up (single member is often an intentional SFP+ uplink).
        assert_eq!(single.severity, SEV_WARN);
        assert_eq!(verdict_tone(single), Some("neutral"));
    }

    #[test]
    fn one_down_uplink_member_yields_a_focused_verdict() {
        // A 2-member LACP uplink to core1 with one member physically down: the
        // collector reports it as an unbundled member (empty actor state) whose
        // interface is oper-down.
        let pc = json::ApiPortChannel {
            ifindex: 5001,
            name: Some("Po1".to_string()),
            alias: None,
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
            alias: None,
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
            alias: None,
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
