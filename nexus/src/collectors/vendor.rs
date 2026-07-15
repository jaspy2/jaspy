// Vendor-aware SNMP source selection, shared by the entity (STP) and VLAN
// collectors.
//
// Both collectors face the same problem: the data they need lives in
// different MIBs depending on the switch vendor (Cisco private MIBs vs
// standard Q-BRIDGE vs HP-ICF-RPVST), and the only reliable applicability
// test is asking the device. Each feature declares its alternatives as
// `Source` impls; `collect_first` probes them in order and remembers the
// winner per device, so steady-state cycles go straight to the right MIB
// instead of re-walking the losing candidates. The first-cycle probe order
// is seeded from the vendor hint derived from discovery data
// (`Device::os_info` / `device_type`) already persisted in the DB.
//
// Adding a vendor = one `Source` impl per feature (its `collect` returns
// None when the device doesn't answer that MIB) + an entry in that
// collector's source list; nothing else changes.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Cisco,
    HpProcurve,
    // Standards-based sources (Q-BRIDGE) and devices whose discovery data
    // matches no known vendor.
    Generic,
}

// Best-effort vendor guess from the persisted discovery fields: os_info is
// sysDescr ("Cisco IOS Software, ...", "HP J9774A 2530-8G Switch, ...",
// "ProCurve J9085A ...", "Aruba JL258A ..."), device_type the ENTITY-MIB
// model string. A wrong or Generic hint only costs wasted first-cycle
// probes, never wrong data — every source still verifies by asking.
pub fn vendor_hint(os_info: Option<&str>, device_type: Option<&str>) -> Vendor {
    let haystack = format!("{} {}", os_info.unwrap_or(""), device_type.unwrap_or("")).to_lowercase();
    if haystack.contains("cisco") {
        return Vendor::Cisco;
    }
    if haystack.contains("procurve")
        || haystack.contains("aruba")
        || haystack.contains("hewlett")
        || haystack.split_whitespace().any(|token| token == "hp")
    {
        return Vendor::HpProcurve;
    }
    Vendor::Generic
}

// One alternative way to collect a feature's data. `Ctx` carries whatever
// the feature's collectors need (snmpbot URL, host addressing, device
// metadata); `collect` returns None when the source doesn't apply to this
// device (its MIB is absent or empty) so selection moves on.
pub trait Source<Ctx>: Sync {
    type Output;
    fn name(&self) -> &'static str;
    fn vendor(&self) -> Vendor;
    fn collect(&self, ctx: &Ctx) -> Option<Self::Output>;
}

// Winning source name per device fqdn. Each collector owns one (STP and
// VLAN winners are independent — a device can be Cisco for one and answer
// only standard MIBs for the other).
pub struct SourceCache {
    winners: Mutex<HashMap<String, &'static str>>,
}

impl SourceCache {
    pub fn new() -> SourceCache {
        SourceCache { winners: Mutex::new(HashMap::new()) }
    }

    fn winner(&self, fqdn: &str) -> Option<&'static str> {
        match self.winners.lock() {
            Ok(winners) => winners.get(fqdn).copied(),
            Err(_) => None,
        }
    }

    fn remember(&self, fqdn: &str, name: &'static str) {
        if let Ok(mut winners) = self.winners.lock() {
            winners.insert(fqdn.to_string(), name);
        }
    }

    fn forget(&self, fqdn: &str) {
        if let Ok(mut winners) = self.winners.lock() {
            winners.remove(fqdn);
        }
    }

    // Drop cache entries for devices no longer monitored.
    pub fn retain(&self, keep: &HashSet<String>) {
        if let Ok(mut winners) = self.winners.lock() {
            winners.retain(|fqdn, _| keep.contains(fqdn));
        }
    }
}

// Try the sources in preference order and return the first answer.
//
// Order: the cached winner first; then, when the hint names a vendor,
// hint-matching sources, standards-based (Generic) sources, and other
// vendors' sources last; a Generic hint keeps the declared order. The
// cached winner is remembered on success. When nothing answers (device
// offline, or it lost the feature) the cache entry is dropped so the next
// cycle re-probes from the hint order.
pub fn collect_first<Ctx, T>(
    cache: &SourceCache,
    fqdn: &str,
    hint: Vendor,
    sources: &[&dyn Source<Ctx, Output = T>],
    ctx: &Ctx,
) -> Option<T> {
    let cached = cache.winner(fqdn);
    let mut order: Vec<usize> = (0..sources.len()).collect();
    order.sort_by_key(|&i| {
        let source = sources[i];
        if Some(source.name()) == cached {
            0u8
        } else if hint == Vendor::Generic {
            1
        } else if source.vendor() == hint {
            1
        } else if source.vendor() == Vendor::Generic {
            2
        } else {
            3
        }
    });
    for i in order {
        if let Some(result) = sources[i].collect(ctx) {
            cache.remember(fqdn, sources[i].name());
            return Some(result);
        }
    }
    cache.forget(fqdn);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test source: records its probe in the ctx call log, answers iff its
    // name is in the ctx answer set.
    struct Fake {
        name: &'static str,
        vendor: Vendor,
    }

    struct Log {
        calls: Mutex<Vec<&'static str>>,
        answers: HashSet<&'static str>,
    }

    impl Log {
        fn answering(answers: &[&'static str]) -> Log {
            Log { calls: Mutex::new(Vec::new()), answers: answers.iter().copied().collect() }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Source<Log> for Fake {
        type Output = &'static str;
        fn name(&self) -> &'static str {
            self.name
        }
        fn vendor(&self) -> Vendor {
            self.vendor
        }
        fn collect(&self, ctx: &Log) -> Option<&'static str> {
            ctx.calls.lock().unwrap().push(self.name);
            if ctx.answers.contains(self.name) { Some(self.name) } else { None }
        }
    }

    const CISCO: Fake = Fake { name: "cisco", vendor: Vendor::Cisco };
    const QBRIDGE: Fake = Fake { name: "q-bridge", vendor: Vendor::Generic };
    const HP: Fake = Fake { name: "hp", vendor: Vendor::HpProcurve };

    fn sources() -> [&'static dyn Source<Log, Output = &'static str>; 3] {
        [&CISCO, &QBRIDGE, &HP]
    }

    // --- vendor_hint ---

    #[test]
    fn hint_matches_cisco_sysdescr_and_models() {
        assert_eq!(vendor_hint(Some("Cisco IOS Software, C2960X ..."), None), Vendor::Cisco);
        assert_eq!(vendor_hint(None, Some("WS-C2960CX-8TC-L,cisco"), ), Vendor::Cisco);
    }

    #[test]
    fn hint_matches_hp_procurve_and_aruba() {
        assert_eq!(vendor_hint(Some("HP J9774A 2530-8G Switch, revision YA.15.16"), None), Vendor::HpProcurve);
        assert_eq!(vendor_hint(Some("ProCurve J9085A Switch 2610-24"), None), Vendor::HpProcurve);
        assert_eq!(vendor_hint(Some("Aruba JL258A 2930F Switch"), None), Vendor::HpProcurve);
        assert_eq!(vendor_hint(Some("Hewlett-Packard Company something"), None), Vendor::HpProcurve);
    }

    #[test]
    fn hint_unknown_is_generic_and_hp_needs_a_word_boundary() {
        assert_eq!(vendor_hint(None, None), Vendor::Generic);
        assert_eq!(vendor_hint(Some("UNKNOWN"), Some("UNKNOWN")), Vendor::Generic);
        // "hp" must be its own token: no false positive on e.g. "dhcp-relay".
        assert_eq!(vendor_hint(Some("switchOS with dhcp-relay"), None), Vendor::Generic);
    }

    // --- collect_first ordering + cache ---

    #[test]
    fn generic_hint_keeps_declared_order() {
        let cache = SourceCache::new();
        let ctx = Log::answering(&["q-bridge"]);
        let won = collect_first(&cache, "sw1", Vendor::Generic, &sources(), &ctx);
        assert_eq!(won, Some("q-bridge"));
        assert_eq!(ctx.calls(), vec!["cisco", "q-bridge"], "declared order, stops at first answer");
    }

    #[test]
    fn vendor_hint_probes_that_vendor_first_then_generic() {
        let cache = SourceCache::new();
        let ctx = Log::answering(&[]);
        collect_first(&cache, "sw1", Vendor::HpProcurve, &sources(), &ctx);
        assert_eq!(ctx.calls(), vec!["hp", "q-bridge", "cisco"], "hinted, then generic, other vendors last");
    }

    #[test]
    fn winner_is_cached_and_probed_first_next_cycle() {
        let cache = SourceCache::new();
        let ctx = Log::answering(&["hp"]);
        collect_first(&cache, "sw1", Vendor::Generic, &sources(), &ctx);
        assert_eq!(ctx.calls(), vec!["cisco", "q-bridge", "hp"]);

        // Second cycle: straight to the winner, no other probes.
        let ctx = Log::answering(&["hp"]);
        collect_first(&cache, "sw1", Vendor::Generic, &sources(), &ctx);
        assert_eq!(ctx.calls(), vec!["hp"]);
    }

    #[test]
    fn cache_is_per_device() {
        let cache = SourceCache::new();
        collect_first(&cache, "sw1", Vendor::Generic, &sources(), &Log::answering(&["hp"]));

        let ctx = Log::answering(&["cisco"]);
        collect_first(&cache, "sw2", Vendor::Generic, &sources(), &ctx);
        assert_eq!(ctx.calls(), vec!["cisco"], "sw1's winner does not leak to sw2");
    }

    #[test]
    fn stale_winner_falls_through_and_cache_updates() {
        let cache = SourceCache::new();
        collect_first(&cache, "sw1", Vendor::Generic, &sources(), &Log::answering(&["cisco"]));

        // Device swapped OS: cisco no longer answers, q-bridge does.
        let ctx = Log::answering(&["q-bridge"]);
        let won = collect_first(&cache, "sw1", Vendor::Generic, &sources(), &ctx);
        assert_eq!(won, Some("q-bridge"));

        let ctx = Log::answering(&["q-bridge"]);
        collect_first(&cache, "sw1", Vendor::Generic, &sources(), &ctx);
        assert_eq!(ctx.calls(), vec!["q-bridge"], "new winner cached");
    }

    #[test]
    fn all_none_forgets_the_winner() {
        let cache = SourceCache::new();
        collect_first(&cache, "sw1", Vendor::HpProcurve, &sources(), &Log::answering(&["cisco"]));

        // Offline cycle: nothing answers.
        assert_eq!(collect_first(&cache, "sw1", Vendor::HpProcurve, &sources(), &Log::answering(&[])), None);

        // Back online: probes from the hint order again, not the stale winner.
        let ctx = Log::answering(&["hp"]);
        collect_first(&cache, "sw1", Vendor::HpProcurve, &sources(), &ctx);
        assert_eq!(ctx.calls(), vec!["hp"]);
    }

    #[test]
    fn retain_drops_unmonitored_devices() {
        let cache = SourceCache::new();
        cache.remember("keep.example.com", "cisco");
        cache.remember("drop.example.com", "cisco");
        let keep: HashSet<String> = vec!["keep.example.com".to_string()].into_iter().collect();
        cache.retain(&keep);
        assert_eq!(cache.winner("keep.example.com"), Some("cisco"));
        assert_eq!(cache.winner("drop.example.com"), None);
    }
}
