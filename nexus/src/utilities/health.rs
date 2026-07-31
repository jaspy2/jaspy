// Per-interface health signal store for field debugging.
//
// nexus keeps only the latest counter snapshot per interface in IMDS; this
// module adds a small in-memory time-series over the *recent* past so the UI
// can answer "how many discards in the last 5 minutes?" or "is this port
// flapping?" without Grafana. It is fed from the single choke point
// `IMDS::report_interfaces` (both poll- and trap-driven samples flow through
// there) and read back by the /api/v1 device routes.
//
// The type is deliberately pure and self-contained: every method that cares
// about time takes `now` (epoch millis, matching `tools::get_time_msecs()`) as
// an argument rather than reading the clock, so the windowing/expiry logic is
// unit-testable with hand-picked timestamps (the same testability idiom the
// entitypoller uses). No I/O, no locking — the owning IMDS provides both.
use std::collections::{HashMap, HashSet, VecDeque};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct HealthConfig {
    // Window for summed counters (discards, errors).
    pub counter_window_ms: u64,
    // Window for flapping + speed-change events.
    pub flap_window_ms: u64,
    // Window for the utilization peak.
    pub util_window_ms: u64,
    // Window for the average-throughput figure shown in the detail view.
    pub throughput_window_ms: u64,
    // Flag high utilization when the windowed peak reaches this percent.
    pub util_threshold_pct: f64,
    // Flag "stale" when the interface has not been polled for longer than this.
    pub stale_ms: u64,
    // A signal is only surfaced when its windowed count strictly exceeds the
    // matching show-threshold (so a single stray error can be tuned out).
    pub error_show_threshold: u64,
    pub discard_show_threshold: u64,
    // Flapping is a count of *recoveries* (down->up transitions) in the window;
    // the signal trips when that count strictly exceeds this threshold. Default
    // 1 means "needs >= 2 recoveries", so a first cable plug-in or a one-off
    // reboot (a single recovery) is not flapping — only a link that keeps
    // coming back is.
    pub flap_show_threshold: u32,
}

impl Default for HealthConfig {
    fn default() -> Self {
        HealthConfig {
            counter_window_ms: 300_000,   // 5 min
            flap_window_ms: 600_000,      // 10 min
            util_window_ms: 300_000,      // 5 min
            throughput_window_ms: 60_000, // 1 min
            util_threshold_pct: 90.0,
            stale_ms: 60_000, // 60 s
            error_show_threshold: 0,
            discard_show_threshold: 0,
            flap_show_threshold: 1, // >= 2 recoveries; see field doc
        }
    }
}

impl HealthConfig {
    // Samples must be retained as long as the longest window that reads them.
    fn sample_retain_ms(&self) -> u64 {
        self.counter_window_ms.max(self.util_window_ms).max(self.throughput_window_ms)
    }
}

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Warn,
    Bad,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Warn => "warn",
            Severity::Bad => "bad",
        }
    }

    // Bad dominates Warn when rolling several signals/interfaces together.
    fn combine(self, other: Severity) -> Severity {
        if self == Severity::Bad || other == Severity::Bad {
            Severity::Bad
        } else {
            Severity::Warn
        }
    }
}

// ---------------------------------------------------------------------------
// Escalation level
// ---------------------------------------------------------------------------

// Per-VLAN policy that decides which of an interface's signals reach the
// device-list rollup badge. It does NOT touch the per-interface badges on the
// device-detail page (those always show every signal) — only what escalates to
// the fleet list. The owning route resolves an interface's level from its access
// VLAN before calling `device_rollup`; trunks and unknown VLANs resolve to
// `Normal`, so the default reproduces today's behavior exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EscalationLevel {
    // Only hard faults (flapping / stale) escalate; minor signals (discards,
    // errors, high-util, renegotiation) stay off the fleet list.
    Quiet,
    // Today's behavior: hard faults -> red, minor signals -> yellow.
    #[default]
    Normal,
    // Minor signals are promoted to red so they are impossible to miss.
    Sensitive,
}

impl EscalationLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            EscalationLevel::Quiet => "quiet",
            EscalationLevel::Normal => "normal",
            EscalationLevel::Sensitive => "sensitive",
        }
    }

    // Parse the wire/stored string; unknown (incl. "normal") -> Normal.
    pub fn from_str(s: &str) -> EscalationLevel {
        match s {
            "quiet" => EscalationLevel::Quiet,
            "sensitive" => EscalationLevel::Sensitive,
            _ => EscalationLevel::Normal,
        }
    }

    // How one interface's health summary contributes to the device rollup under
    // this level. `hard` is the Bad-class signal set (flapping / stale).
    fn contribution(self, severity: Option<Severity>, hard: bool) -> Option<Severity> {
        match self {
            EscalationLevel::Quiet => {
                if hard {
                    Some(Severity::Bad)
                } else {
                    None
                }
            }
            EscalationLevel::Normal => severity,
            // Any tripped signal becomes red (hard faults already are).
            EscalationLevel::Sensitive => severity.map(|_| Severity::Bad),
        }
    }
}

// ---------------------------------------------------------------------------
// Stored samples/events (private; serde for optional disk persistence)
// ---------------------------------------------------------------------------

// One accepted poll's per-interval increments. Deltas are computed by the
// caller (new - old) so a counter reset lands as 0, consistent with the
// existing `validate_counters` guard in IMDS.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Sample {
    ts: u64,
    in_errors: u64,
    out_errors: u64,
    out_discards: u64,
    in_octets: u64,  // delta bytes since previous accepted sample
    out_octets: u64, // delta bytes
    interval_ms: u64,
    // Effective speed (Mbps) in force during this interval; utilization is
    // computed against the sample's own speed so a later downshift can't make
    // an old high-throughput sample read as >100%.
    speed_mbps: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct FlapEvent {
    ts: u64,
    up: bool, // state entered at this transition
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SpeedEvent {
    ts: u64,
    from: Option<i32>,
    to: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct InterfaceHealth {
    samples: VecDeque<Sample>,
    flaps: VecDeque<FlapEvent>,
    speed_changes: VecDeque<SpeedEvent>,
}

// ---------------------------------------------------------------------------
// Ingest input
// ---------------------------------------------------------------------------

// What `report_interfaces` hands us per interface per report. Counter fields
// are already-computed deltas; `interval_ms == 0` means "no measurable
// interval" (first sample, or a trap that carries no counters) and suppresses
// the counter/utilization sample while still recording any transition.
#[derive(Clone, Debug, Default)]
pub struct SampleInput {
    pub in_errors: u64,
    pub out_errors: u64,
    pub out_discards: u64,
    pub in_octets: u64,
    pub out_octets: u64,
    pub interval_ms: u64,
    pub up_transition: Option<bool>,        // Some(new_state) when oper status changed
    pub speed_change: Option<(Option<i32>, i32)>, // (from, to) when speed changed
    pub speed_mbps: Option<i32>,            // current effective speed, if known
}

// ---------------------------------------------------------------------------
// Summary (read side)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceHealthSummary {
    // None = healthy (no badge); Some = a signal tripped. The metric fields
    // below are always populated so the UI can show the full picture (incl.
    // the zeroed/OK metrics) for any polled interface.
    pub severity: Option<Severity>,
    // Number of recoveries (down->up transitions) within the flap window, i.e.
    // how many times the link came back up. A first plug-in or a single reboot
    // reads as 1; a genuinely flapping link reads higher.
    pub flap_count: u32,
    // Whether flap_count crossed the configured threshold (>= 2 recoveries by
    // default). Exposed so the UI/issues layer renders the alert off the
    // server-side decision instead of re-deriving it — mirrors high_utilization.
    pub flapping: bool,
    // Time since the link last came back up (last recovery), within the window.
    pub last_flap_secs_ago: Option<u64>,
    pub in_errors: u64,
    pub out_errors: u64,
    pub discards: u64,
    // Whether windowed discards crossed the configured threshold. Exposed like
    // high_utilization so the UI can render a dedicated discard icon off the
    // server-side decision rather than the raw count.
    pub discards_high: bool,
    pub speed_change_count: u32,
    pub last_speed_change: Option<(Option<i32>, i32)>,
    pub peak_utilization_pct: Option<f64>,
    // Whether peak utilization crossed the configured threshold (the frontend
    // renders the utilization line off this, so the threshold stays server-side).
    pub high_utilization: bool,
    // Average throughput over the throughput window (bits/sec), rx and tx.
    // None when there aren't enough samples to measure a rate.
    pub rx_bps_avg: Option<f64>,
    pub tx_bps_avg: Option<f64>,
    pub stale: bool,
    pub counter_window_secs: u64,
    pub flap_window_secs: u64,
    pub util_window_secs: u64,
    pub throughput_window_secs: u64,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct HealthStore {
    devices: HashMap<String, HashMap<i32, InterfaceHealth>>, // fqdn -> ifindex
    cfg: HealthConfig,
}

impl HealthStore {
    pub fn new(cfg: HealthConfig) -> HealthStore {
        HealthStore { devices: HashMap::new(), cfg }
    }

    // Record one interface report. Pushes a counter sample only when the report
    // carried a measurable interval; always records transitions.
    pub fn ingest(&mut self, fqdn: &str, ifindex: i32, input: SampleInput, now: u64) {
        let ih = self
            .devices
            .entry(fqdn.to_string())
            .or_default()
            .entry(ifindex)
            .or_default();

        if input.interval_ms > 0 {
            ih.samples.push_back(Sample {
                ts: now,
                in_errors: input.in_errors,
                out_errors: input.out_errors,
                out_discards: input.out_discards,
                in_octets: input.in_octets,
                out_octets: input.out_octets,
                interval_ms: input.interval_ms,
                speed_mbps: input.speed_mbps,
            });
        }
        if let Some(up) = input.up_transition {
            ih.flaps.push_back(FlapEvent { ts: now, up });
        }
        if let Some((from, to)) = input.speed_change {
            ih.speed_changes.push_back(SpeedEvent { ts: now, from, to });
        }

        Self::prune(ih, &self.cfg, now);
    }

    // Drop entries older than the windows that read them; bounds memory to
    // ~window/poll_interval samples per interface.
    fn prune(ih: &mut InterfaceHealth, cfg: &HealthConfig, now: u64) {
        let sample_cut = now.saturating_sub(cfg.sample_retain_ms());
        while ih.samples.front().map_or(false, |s| s.ts < sample_cut) {
            ih.samples.pop_front();
        }
        let flap_cut = now.saturating_sub(cfg.flap_window_ms);
        while ih.flaps.front().map_or(false, |f| f.ts < flap_cut) {
            ih.flaps.pop_front();
        }
        while ih.speed_changes.front().map_or(false, |e| e.ts < flap_cut) {
            ih.speed_changes.pop_front();
        }
    }

    // Health summary for one interface: `None` only when the interface is
    // unknown to the store (never polled). For a known interface it always
    // returns the full metrics, with `severity: None` when nothing tripped —
    // so the UI can render the complete detail view for every interface.
    // `last_report` is the interface's last-poll timestamp from IMDS (0 =
    // never polled) and drives the stale check.
    pub fn summary(
        &self,
        fqdn: &str,
        ifindex: i32,
        now: u64,
        last_report: u64,
    ) -> Option<InterfaceHealthSummary> {
        let ih = self.devices.get(fqdn)?.get(&ifindex)?;
        let cfg = &self.cfg;

        let counter_cut = now.saturating_sub(cfg.counter_window_ms);
        let mut in_errors = 0u64;
        let mut out_errors = 0u64;
        let mut discards = 0u64;
        for s in ih.samples.iter().filter(|s| s.ts >= counter_cut) {
            in_errors += s.in_errors;
            out_errors += s.out_errors;
            discards += s.out_discards;
        }

        let util_cut = now.saturating_sub(cfg.util_window_ms);
        let mut peak_util: Option<f64> = None;
        for s in ih.samples.iter().filter(|s| s.ts >= util_cut && s.interval_ms > 0) {
            if let Some(speed_mbps) = s.speed_mbps {
                if speed_mbps > 0 {
                    let rx = util_pct(s.in_octets, s.interval_ms, speed_mbps);
                    let tx = util_pct(s.out_octets, s.interval_ms, speed_mbps);
                    let sample_peak = rx.max(tx);
                    peak_util = Some(peak_util.map_or(sample_peak, |p: f64| p.max(sample_peak)));
                }
            }
        }

        // Average throughput over the throughput window: total bits over the
        // total measured interval (sum of sample intervals), rx and tx.
        let tp_cut = now.saturating_sub(cfg.throughput_window_ms);
        let mut tp_in_bytes = 0u64;
        let mut tp_out_bytes = 0u64;
        let mut tp_span_ms = 0u64;
        for s in ih.samples.iter().filter(|s| s.ts >= tp_cut && s.interval_ms > 0) {
            tp_in_bytes += s.in_octets;
            tp_out_bytes += s.out_octets;
            tp_span_ms += s.interval_ms;
        }
        let (rx_bps_avg, tx_bps_avg) = if tp_span_ms > 0 {
            let span_s = tp_span_ms as f64 / 1000.0;
            (Some(tp_in_bytes as f64 * 8.0 / span_s), Some(tp_out_bytes as f64 * 8.0 / span_s))
        } else {
            (None, None)
        };

        // Window flaps/speed-changes at read time too: prune only runs on
        // ingest, so a stale (no longer polled) or freshly reloaded interface
        // must still age its events out here, or it reports false "Bad".
        // Count only recoveries (down->up transitions): a single one-way change
        // (first plug-in, unplug, one-off reboot) is not flapping. A link that
        // keeps coming back racks up recoveries. `last_flap` is the last such
        // recovery, so it tracks what flap_count measures.
        let flap_cut = now.saturating_sub(cfg.flap_window_ms);
        let recoveries = ih.flaps.iter().filter(|f| f.ts >= flap_cut && f.up).count();
        let flap_count = recoveries as u32;
        let last_flap_secs_ago = ih
            .flaps
            .iter()
            .filter(|f| f.ts >= flap_cut && f.up)
            .last()
            .map(|f| now.saturating_sub(f.ts) / 1000);
        let speed_change_count = ih.speed_changes.iter().filter(|e| e.ts >= flap_cut).count() as u32;
        let last_speed_change = ih.speed_changes.iter().filter(|e| e.ts >= flap_cut).last().map(|e| (e.from, e.to));

        let stale = last_report > 0 && now.saturating_sub(last_report) > cfg.stale_ms;

        let flapping = flap_count > cfg.flap_show_threshold;
        let has_errors = in_errors + out_errors > cfg.error_show_threshold;
        let has_discards = discards > cfg.discard_show_threshold;
        let high_util = peak_util.map_or(false, |p| p >= cfg.util_threshold_pct);
        let renegotiated = speed_change_count > 0;

        // None = healthy. Flapping and a device that stopped answering are the
        // serious (Bad) ones; the rest are Warn.
        let severity = if flapping || stale {
            Some(Severity::Bad)
        } else if has_errors || has_discards || high_util || renegotiated {
            Some(Severity::Warn)
        } else {
            None
        };

        Some(InterfaceHealthSummary {
            severity,
            flap_count,
            flapping,
            last_flap_secs_ago,
            in_errors,
            out_errors,
            discards,
            discards_high: has_discards,
            speed_change_count,
            last_speed_change,
            peak_utilization_pct: peak_util,
            high_utilization: high_util,
            rx_bps_avg,
            tx_bps_avg,
            stale,
            counter_window_secs: cfg.counter_window_ms / 1000,
            flap_window_secs: cfg.flap_window_ms / 1000,
            util_window_secs: cfg.util_window_ms / 1000,
            throughput_window_secs: cfg.throughput_window_ms / 1000,
        })
    }

    // Worst severity across a device's interfaces, for the /devices list badge.
    // `last_reports` maps ifindex -> last-poll timestamp (from IMDS). `levels`
    // maps ifindex -> per-VLAN escalation policy; an interface absent from the
    // map (VLAN unknown, trunk, or no policy configured) defaults to `Normal`,
    // reproducing today's behavior. The level filters which of the interface's
    // signals contribute to this rollup — the per-interface `summary()` badges
    // are unaffected.
    pub fn device_rollup(
        &self,
        fqdn: &str,
        now: u64,
        last_reports: &HashMap<i32, u64>,
        levels: &HashMap<i32, EscalationLevel>,
    ) -> Option<Severity> {
        let interfaces = self.devices.get(fqdn)?;
        let mut worst: Option<Severity> = None;
        for ifindex in interfaces.keys() {
            let last_report = last_reports.get(ifindex).copied().unwrap_or(0);
            let Some(summary) = self.summary(fqdn, *ifindex, now, last_report) else {
                continue;
            };
            let level = levels.get(ifindex).copied().unwrap_or_default();
            let hard = summary.flapping || summary.stale;
            if let Some(severity) = level.contribution(summary.severity, hard) {
                worst = Some(match worst {
                    Some(w) => w.combine(severity),
                    None => severity,
                });
            }
        }
        worst
    }

    // GC devices no longer monitored (mirrors IMDS::retain_devices).
    pub fn retain_devices(&mut self, keep: &HashSet<String>) {
        self.devices.retain(|fqdn, _| keep.contains(fqdn));
    }

    // GC per-interface history for interfaces no longer in the DB (mirrors
    // IMDS::retain_interfaces). No-op for a device with no history yet.
    pub fn retain_interfaces(&mut self, fqdn: &str, keep: &HashSet<i32>) {
        if let Some(interfaces) = self.devices.get_mut(fqdn) {
            interfaces.retain(|ifindex, _| keep.contains(ifindex));
        }
    }

    // Drop all recent history for a device's interfaces — the samples belong to
    // hardware that was just replaced (IMDS::refresh_device base_mac reset).
    // No-op for a device with no history yet.
    pub fn forget_device_interfaces(&mut self, fqdn: &str) {
        self.devices.remove(fqdn);
    }

    // --- optional disk persistence: only the sample data, never the config ---

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.devices)
    }

    pub fn load_json(&mut self, json: &str) -> Result<(), serde_json::Error> {
        self.devices = serde_json::from_str(json)?;
        Ok(())
    }
}

// Utilization percent for one direction: bytes over interval_ms against a
// speed in Mbps. bits / (interval_ms * speed_mbps * 10) * ... derived so that a
// full line for the whole interval reads 100%.
fn util_pct(octets: u64, interval_ms: u64, speed_mbps: i32) -> f64 {
    if interval_ms == 0 || speed_mbps <= 0 {
        return 0.0;
    }
    let bits = octets as f64 * 8.0;
    let capacity_bits = speed_mbps as f64 * 1_000_000.0 * (interval_ms as f64 / 1000.0);
    if capacity_bits <= 0.0 {
        return 0.0;
    }
    bits / capacity_bits * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HealthConfig {
        // Explicit, round windows for deterministic assertions.
        HealthConfig {
            counter_window_ms: 300_000,
            flap_window_ms: 600_000,
            util_window_ms: 300_000,
            throughput_window_ms: 60_000,
            util_threshold_pct: 90.0,
            stale_ms: 60_000,
            error_show_threshold: 0,
            discard_show_threshold: 0,
            flap_show_threshold: 1, // >= 2 recoveries trips (matches default)
        }
    }

    fn store() -> HealthStore {
        HealthStore::new(cfg())
    }

    fn counters(in_errors: u64, out_errors: u64, out_discards: u64, interval_ms: u64) -> SampleInput {
        SampleInput { in_errors, out_errors, out_discards, interval_ms, ..Default::default() }
    }

    const FQDN: &str = "sw1.example.com";

    // --- counters (discards / errors) ---

    #[test]
    fn discards_summed_within_window() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 5000, 10_000), 1_000_000);
        s.ingest(FQDN, 1, counters(0, 0, 512, 10_000), 1_010_000);
        let summary = s.summary(FQDN, 1, 1_010_000, 1_010_000).unwrap();
        assert_eq!(summary.discards, 5512);
        assert_eq!(summary.severity, Some(Severity::Warn));
    }

    #[test]
    fn errors_summed_in_and_out() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(3, 4, 0, 10_000), 1_000_000);
        let summary = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert_eq!(summary.in_errors, 3);
        assert_eq!(summary.out_errors, 4);
    }

    #[test]
    fn counter_samples_outside_window_are_excluded() {
        let mut s = store();
        // Old sample well before the 5-min counter window.
        s.ingest(FQDN, 1, counters(0, 0, 9999, 10_000), 1_000_000);
        // Recent sample inside the window.
        s.ingest(FQDN, 1, counters(0, 0, 7, 10_000), 1_000_000 + 400_000);
        let now = 1_000_000 + 400_000;
        let summary = s.summary(FQDN, 1, now, now).unwrap();
        // The 9999 sample is >5 min old; only the 7 counts.
        assert_eq!(summary.discards, 7);
    }

    #[test]
    fn intervalless_samples_record_no_counters() {
        let mut s = store();
        // interval_ms == 0 (e.g. a trap with no counters): no counter sample
        // pushed, so the windowed counters stay zero and severity is healthy.
        s.ingest(FQDN, 1, counters(5, 5, 5, 0), 1_000_000);
        let summary = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert_eq!(summary.discards, 0);
        assert_eq!(summary.severity, None);
    }

    #[test]
    fn healthy_interface_has_no_severity_but_still_summarizes() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 0, 10_000), 1_000_000);
        // Known interface with nothing wrong: metrics present, no badge.
        let summary = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert_eq!(summary.severity, None);
        assert_eq!(summary.discards, 0);
    }

    #[test]
    fn show_thresholds_suppress_small_counts() {
        let mut c = cfg();
        c.discard_show_threshold = 10;
        let mut s = HealthStore::new(c);
        s.ingest(FQDN, 1, counters(0, 0, 5, 10_000), 1_000_000);
        // 5 discards <= threshold 10 -> no badge (but the count is still shown),
        // and discards_high tracks the same threshold decision.
        let below = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert_eq!(below.severity, None);
        assert!(!below.discards_high);
        s.ingest(FQDN, 1, counters(0, 0, 20, 10_000), 1_000_000);
        // now 25 > 10 -> surfaced.
        let above = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert_eq!(above.severity, Some(Severity::Warn));
        assert_eq!(above.discards, 25);
        assert!(above.discards_high);
    }

    // --- flapping ---

    fn flap(up: bool) -> SampleInput {
        SampleInput { up_transition: Some(up), ..Default::default() }
    }

    #[test]
    fn recoveries_counted_within_window_with_last_ago() {
        // Two full down->up->down->up bounces: two recoveries -> flapping.
        // flap_count counts recoveries (the two `up`s), last_flap tracks the
        // most recent recovery, not the trailing `down`.
        let mut s = store();
        s.ingest(FQDN, 1, flap(false), 1_000_000);
        s.ingest(FQDN, 1, flap(true), 1_060_000);  // recovery #1
        s.ingest(FQDN, 1, flap(false), 1_120_000);
        s.ingest(FQDN, 1, flap(true), 1_180_000);  // recovery #2
        let now = 1_600_000; // last recovery 420s ago, all within 10-min window
        let summary = s.summary(FQDN, 1, now, now).unwrap();
        assert_eq!(summary.flap_count, 2);
        assert_eq!(summary.last_flap_secs_ago, Some(420));
        assert!(summary.flapping);
        assert_eq!(summary.severity, Some(Severity::Bad));
    }

    #[test]
    fn single_recovery_is_not_flapping() {
        // First cable plug-in: the port was polled down, then comes up once.
        // One recovery is a state change, not a flap — the reported false
        // positive ("flapped 1 time(s)") must not fire.
        let mut s = store();
        s.ingest(FQDN, 1, flap(false), 1_000_000); // polled down (nothing plugged)
        s.ingest(FQDN, 1, flap(true), 1_060_000);  // cable plugged -> up
        let summary = s.summary(FQDN, 1, 1_100_000, 1_100_000).unwrap();
        assert_eq!(summary.flap_count, 1);
        assert!(!summary.flapping);
        assert_eq!(summary.severity, None);
    }

    #[test]
    fn unplug_records_no_recovery() {
        // A one-way down transition (cable pulled / decommission) never came
        // back up: zero recoveries, not flapping.
        let mut s = store();
        s.ingest(FQDN, 1, flap(false), 1_000_000);
        let summary = s.summary(FQDN, 1, 1_050_000, 1_050_000).unwrap();
        assert_eq!(summary.flap_count, 0);
        assert!(!summary.flapping);
        assert_eq!(summary.severity, None);
    }

    #[test]
    fn single_reboot_is_not_flapping() {
        // Far-end device reboots once: the link drops (up->down) and recovers
        // (down->up) a single time. One recovery -> not flapping.
        let mut s = store();
        s.ingest(FQDN, 1, flap(false), 1_000_000); // link dropped
        s.ingest(FQDN, 1, flap(true), 1_030_000);  // came back once
        let summary = s.summary(FQDN, 1, 1_100_000, 1_100_000).unwrap();
        assert_eq!(summary.flap_count, 1);
        assert!(!summary.flapping);
        assert_eq!(summary.severity, None);
    }

    #[test]
    fn recoveries_outside_window_are_pruned() {
        let mut s = store();
        s.ingest(FQDN, 1, flap(false), 1_000_000);
        // A much later ingest prunes the old down (>10 min gap); one recovery
        // survives in-window.
        s.ingest(FQDN, 1, flap(true), 1_000_000 + 700_000);
        let now = 1_000_000 + 700_000;
        let summary = s.summary(FQDN, 1, now, now).unwrap();
        assert_eq!(summary.flap_count, 1);
        assert!(!summary.flapping);
    }

    #[test]
    fn recoveries_age_out_at_read_time_without_new_ingest() {
        // A stale/decommissioned interface stops being ingested, so prune never
        // runs again. summary() must still window the frozen events out, or the
        // interface reports a permanent false "Bad".
        let mut s = store();
        s.ingest(FQDN, 1, flap(false), 1_000_000);
        s.ingest(FQDN, 1, flap(true), 1_010_000);  // recovery #1
        s.ingest(FQDN, 1, flap(false), 1_020_000);
        s.ingest(FQDN, 1, flap(true), 1_030_000);  // recovery #2
        // Read within the flap window: both recoveries present -> flapping.
        let within = s.summary(FQDN, 1, 1_100_000, 1_100_000).unwrap();
        assert_eq!(within.flap_count, 2);
        assert_eq!(within.severity, Some(Severity::Bad));
        // Read past the window with NO further ingest: aged out -> no badge.
        // last_report == now so the stale signal does not fire either.
        let later = 1_030_000 + 700_000;
        let summary = s.summary(FQDN, 1, later, later).unwrap();
        assert_eq!(summary.flap_count, 0);
        assert!(!summary.flapping);
        assert_eq!(summary.severity, None);
    }

    // --- speed renegotiation ---

    #[test]
    fn speed_change_recorded_with_last() {
        let mut s = store();
        s.ingest(
            FQDN,
            1,
            SampleInput { speed_change: Some((Some(1000), 100)), speed_mbps: Some(100), ..Default::default() },
            1_000_000,
        );
        let summary = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert_eq!(summary.speed_change_count, 1);
        assert_eq!(summary.last_speed_change, Some((Some(1000), 100)));
        assert_eq!(summary.severity, Some(Severity::Warn));
    }

    // --- utilization ---

    #[test]
    fn utilization_peak_and_threshold() {
        let mut s = store();
        // 1 Gbps link. In 10 s, 95% util = 0.95 * 1e9 bits/s * 10 s / 8 = bytes.
        let bytes_95 = (0.95 * 1_000_000_000.0 * 10.0 / 8.0) as u64;
        let mut input = counters(0, 0, 0, 10_000);
        input.in_octets = bytes_95;
        input.speed_mbps = Some(1000);
        s.ingest(FQDN, 1, input, 1_000_000);
        let summary = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        let peak = summary.peak_utilization_pct.unwrap();
        assert!((peak - 95.0).abs() < 0.5, "peak was {}", peak);
        assert!(summary.high_utilization);
        assert_eq!(summary.severity, Some(Severity::Warn));
    }

    #[test]
    fn utilization_uses_per_sample_speed_after_downshift() {
        // A sample captured at 1 Gbps (95% util) followed by a downshift to
        // 100 Mbps must be scored against its own speed, not the latest — else
        // the old sample reads as ~950% and falsely trips high-util.
        let mut s = store();
        let mut fast = counters(0, 0, 0, 10_000);
        fast.in_octets = (0.95 * 1_000_000_000.0 * 10.0 / 8.0) as u64;
        fast.speed_mbps = Some(1000);
        s.ingest(FQDN, 1, fast, 1_000_000);
        let mut slow = counters(0, 0, 0, 10_000);
        slow.in_octets = 1_000_000; // trivial traffic on the 100 Mbps link
        slow.speed_mbps = Some(100);
        s.ingest(FQDN, 1, slow, 1_010_000);

        let peak = s.summary(FQDN, 1, 1_010_000, 1_010_000).unwrap().peak_utilization_pct.unwrap();
        assert!(peak > 90.0 && peak < 100.0, "peak should stay ~95%, was {}", peak);
    }

    #[test]
    fn average_throughput_over_window() {
        let mut s = store();
        // Two 10 s samples: 10 MB in, 1 MB out each. Over 20 s that's
        // 20 MB * 8 / 20 s = 8 Mbps rx, 0.8 Mbps tx.
        for t in [1_000_000u64, 1_010_000] {
            let mut input = counters(0, 0, 0, 10_000);
            input.in_octets = 10_000_000;
            input.out_octets = 1_000_000;
            s.ingest(FQDN, 1, input, t);
        }
        let summary = s.summary(FQDN, 1, 1_010_000, 1_010_000).unwrap();
        let rx = summary.rx_bps_avg.unwrap();
        let tx = summary.tx_bps_avg.unwrap();
        assert!((rx - 8_000_000.0).abs() < 1.0, "rx bps was {}", rx);
        assert!((tx - 800_000.0).abs() < 1.0, "tx bps was {}", tx);
        // Throughput alone is not a health signal.
        assert_eq!(summary.severity, None);
    }

    #[test]
    fn throughput_none_without_samples() {
        let mut s = store();
        s.ingest(FQDN, 1, flap(false), 1_000_000); // event only, no counter sample
        let summary = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert_eq!(summary.rx_bps_avg, None);
        assert_eq!(summary.tx_bps_avg, None);
    }

    #[test]
    fn low_utilization_not_flagged() {
        let mut s = store();
        let mut input = counters(0, 0, 0, 10_000);
        input.in_octets = 50_000; // ~40 kbps on a 1G link
        input.speed_mbps = Some(1000);
        s.ingest(FQDN, 1, input, 1_000_000);
        let summary = s.summary(FQDN, 1, 1_000_000, 1_000_000).unwrap();
        assert!(!summary.high_utilization);
        assert_eq!(summary.severity, None);
    }

    // --- stale ---

    #[test]
    fn stale_flagged_when_last_report_old() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 0, 10_000), 1_000_000);
        // now is 90 s after last_report (> 60 s stale threshold).
        let now = 1_000_000 + 90_000;
        let summary = s.summary(FQDN, 1, now, 1_000_000).unwrap();
        assert!(summary.stale);
        assert_eq!(summary.severity, Some(Severity::Bad));
    }

    #[test]
    fn never_polled_is_not_stale() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 0, 10_000), 1_000_000);
        // last_report == 0 means "never polled", not stale -> no badge.
        let summary = s.summary(FQDN, 1, 5_000_000, 0).unwrap();
        assert!(!summary.stale);
        assert_eq!(summary.severity, None);
    }

    // --- device rollup ---

    #[test]
    fn device_rollup_returns_worst_severity() {
        let mut s = store();
        // iface 1: warn (discards). iface 2: bad (flapping: 2 recoveries).
        // iface 3: healthy.
        s.ingest(FQDN, 1, counters(0, 0, 100, 10_000), 1_000_000);
        s.ingest(FQDN, 2, flap(false), 1_000_000);
        s.ingest(FQDN, 2, flap(true), 1_000_000);
        s.ingest(FQDN, 2, flap(false), 1_000_000);
        s.ingest(FQDN, 2, flap(true), 1_000_000);
        s.ingest(FQDN, 3, counters(0, 0, 0, 10_000), 1_000_000);
        let last: HashMap<i32, u64> = vec![(1, 1_000_000u64), (2, 1_000_000), (3, 1_000_000)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &HashMap::new()), Some(Severity::Bad));
    }

    #[test]
    fn device_rollup_none_when_all_healthy() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 0, 10_000), 1_000_000);
        let last: HashMap<i32, u64> = vec![(1, 1_000_000u64)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &HashMap::new()), None);
    }

    // --- per-VLAN escalation levels ---

    #[test]
    fn quiet_level_suppresses_minor_but_keeps_hard_faults() {
        let mut s = store();
        // iface 1: warn (discards only). iface 2: bad (flapping).
        s.ingest(FQDN, 1, counters(0, 0, 100, 10_000), 1_000_000);
        s.ingest(FQDN, 2, flap(false), 1_000_000);
        s.ingest(FQDN, 2, flap(true), 1_000_000);
        s.ingest(FQDN, 2, flap(false), 1_000_000);
        s.ingest(FQDN, 2, flap(true), 1_000_000);
        let last: HashMap<i32, u64> = vec![(1, 1_000_000u64), (2, 1_000_000)].into_iter().collect();

        // Quiet on iface 1 alone: its discard warning is dropped, iface 2 still red.
        let levels: HashMap<i32, EscalationLevel> = vec![(1, EscalationLevel::Quiet)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &levels), Some(Severity::Bad));

        // Quiet on both: iface 1 minor suppressed, but iface 2 flapping is a hard
        // fault and still escalates.
        let levels: HashMap<i32, EscalationLevel> =
            vec![(1, EscalationLevel::Quiet), (2, EscalationLevel::Quiet)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &levels), Some(Severity::Bad));
    }

    #[test]
    fn quiet_level_silences_a_discards_only_device() {
        let mut s = store();
        // Single interface, discards only -> normally warn (yellow).
        s.ingest(FQDN, 1, counters(0, 0, 100, 10_000), 1_000_000);
        let last: HashMap<i32, u64> = vec![(1, 1_000_000u64)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &HashMap::new()), Some(Severity::Warn));
        let levels: HashMap<i32, EscalationLevel> = vec![(1, EscalationLevel::Quiet)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &levels), None);
    }

    #[test]
    fn sensitive_level_promotes_minor_to_red() {
        let mut s = store();
        // Discards only -> warn under Normal, promoted to Bad under Sensitive.
        s.ingest(FQDN, 1, counters(0, 0, 100, 10_000), 1_000_000);
        let last: HashMap<i32, u64> = vec![(1, 1_000_000u64)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &HashMap::new()), Some(Severity::Warn));
        let levels: HashMap<i32, EscalationLevel> = vec![(1, EscalationLevel::Sensitive)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &levels), Some(Severity::Bad));
    }

    #[test]
    fn sensitive_level_leaves_healthy_interface_alone() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 0, 10_000), 1_000_000);
        let last: HashMap<i32, u64> = vec![(1, 1_000_000u64)].into_iter().collect();
        let levels: HashMap<i32, EscalationLevel> = vec![(1, EscalationLevel::Sensitive)].into_iter().collect();
        assert_eq!(s.device_rollup(FQDN, 1_000_000, &last, &levels), None);
    }

    #[test]
    fn escalation_level_from_str_roundtrip() {
        for lvl in [EscalationLevel::Quiet, EscalationLevel::Normal, EscalationLevel::Sensitive] {
            assert_eq!(EscalationLevel::from_str(lvl.as_str()), lvl);
        }
        assert_eq!(EscalationLevel::from_str("bogus"), EscalationLevel::Normal);
        assert_eq!(EscalationLevel::default(), EscalationLevel::Normal);
    }

    // --- retain / unknown keys ---

    #[test]
    fn retain_devices_drops_unmonitored() {
        let mut s = store();
        s.ingest("keep.example.com", 1, counters(0, 0, 1, 10_000), 1_000_000);
        s.ingest("drop.example.com", 1, counters(0, 0, 1, 10_000), 1_000_000);
        let keep: HashSet<String> = vec!["keep.example.com".to_string()].into_iter().collect();
        s.retain_devices(&keep);
        assert!(s.summary("keep.example.com", 1, 1_000_000, 1_000_000).is_some());
        assert!(s.summary("drop.example.com", 1, 1_000_000, 1_000_000).is_none());
    }

    #[test]
    fn retain_interfaces_drops_stale_ifindexes() {
        let mut s = store();
        s.ingest(FQDN, 5, counters(0, 0, 1, 10_000), 1_000_000);
        s.ingest(FQDN, 7, counters(0, 0, 1, 10_000), 1_000_000);
        // DB only knows ifindex 7 now (5 was reindexed/removed).
        let keep: HashSet<i32> = vec![7].into_iter().collect();
        s.retain_interfaces(FQDN, &keep);
        assert!(s.summary(FQDN, 7, 1_000_000, 1_000_000).is_some());
        assert!(s.summary(FQDN, 5, 1_000_000, 1_000_000).is_none());
        // Unknown device is a no-op (must not panic).
        s.retain_interfaces("ghost.example.com", &keep);
    }

    #[test]
    fn forget_device_interfaces_clears_all_history() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 1, 10_000), 1_000_000);
        s.ingest(FQDN, 2, counters(0, 0, 1, 10_000), 1_000_000);
        // Chassis swapped: drop the whole device's history.
        s.forget_device_interfaces(FQDN);
        assert!(s.summary(FQDN, 1, 1_000_000, 1_000_000).is_none());
        assert!(s.summary(FQDN, 2, 1_000_000, 1_000_000).is_none());
        // Unknown device is a no-op (must not panic).
        s.forget_device_interfaces("ghost.example.com");
    }

    #[test]
    fn summary_unknown_device_or_iface_is_none() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(0, 0, 1, 10_000), 1_000_000);
        assert!(s.summary("ghost.example.com", 1, 1_000_000, 1_000_000).is_none());
        assert!(s.summary(FQDN, 99, 1_000_000, 1_000_000).is_none());
    }

    // --- pruning bounds the buffer ---

    #[test]
    fn old_samples_pruned_bounds_buffer() {
        let mut s = store();
        // 100 samples 10 s apart = 1000 s span; only the last ~300 s (30
        // samples) survive the 5-min sample-retain window.
        for i in 0..100u64 {
            s.ingest(FQDN, 1, counters(0, 0, 1, 10_000), 1_000_000 + i * 10_000);
        }
        let ih = s.devices.get(FQDN).unwrap().get(&1).unwrap();
        assert!(ih.samples.len() <= 31, "buffer not bounded: {}", ih.samples.len());
    }

    // --- persistence roundtrip ---

    #[test]
    fn json_roundtrip_preserves_state() {
        let mut s = store();
        s.ingest(FQDN, 1, counters(1, 2, 3, 10_000), 1_000_000);
        s.ingest(FQDN, 1, flap(false), 1_005_000);
        s.ingest(FQDN, 2, SampleInput { speed_change: Some((Some(1000), 100)), ..Default::default() }, 1_000_000);
        let json = s.to_json().unwrap();

        let mut restored = store();
        restored.load_json(&json).unwrap();
        assert_eq!(restored.devices, s.devices);
    }
}
