// Parsing and reconstruction of the snmpbot host addressing forms, in one
// place so both back ends agree on them.
//
// snmpbot accepts three host spellings, and the collectors use two different
// URL conventions for passing the community:
//   * poller & discovery: path host is the bare `fqdn`, community rides in a
//     `?snmp=community@fqdn` query parameter (Style::QueryParam).
//   * entitypoller/vlanpoller/lagpoller: community is inlined into the path
//     host as `community@fqdn`, or `community@vlan@fqdn` for Cisco per-VLAN
//     community indexing (Style::Inline).
//
// The embedded client ignores the URL style and only needs (fqdn, community,
// vlan); the snmpbot HTTP client reconstructs the byte-identical URL from the
// recorded style so it remains a drop-in match against production snmpbot.

#[derive(Debug, Clone, PartialEq)]
enum Style {
    QueryParam,
    Inline,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostSpec {
    pub fqdn: String,
    pub community: Option<String>,
    pub vlan: Option<i64>,
    style: Style,
}

impl HostSpec {
    // Query-param addressing (poller, discovery): the community is known
    // separately and the fqdn is the path host.
    pub fn with_community(fqdn: &str, community: &str) -> HostSpec {
        HostSpec {
            fqdn: fqdn.to_string(),
            community: Some(community.to_string()),
            vlan: None,
            style: Style::QueryParam,
        }
    }

    // Inline addressing (entitypoller/vlanpoller/lagpoller): parse the
    // `community@fqdn` / `community@vlan@fqdn` / `fqdn` host string. Mirrors
    // mock/snmpbot.rs::parse_path.
    pub fn parse(host: &str) -> HostSpec {
        let parts: Vec<&str> = host.split('@').collect();
        match parts.as_slice() {
            [fqdn] => HostSpec { fqdn: (*fqdn).to_string(), community: None, vlan: None, style: Style::Inline },
            [community, fqdn] => HostSpec {
                fqdn: (*fqdn).to_string(),
                community: Some((*community).to_string()),
                vlan: None,
                style: Style::Inline,
            },
            [community, vlan, fqdn] => HostSpec {
                fqdn: (*fqdn).to_string(),
                community: Some((*community).to_string()),
                vlan: vlan.parse::<i64>().ok(),
                style: Style::Inline,
            },
            // More than two '@' is malformed; keep the whole string as the
            // fqdn so the downstream error is a clean SNMP/HTTP failure.
            _ => HostSpec { fqdn: host.to_string(), community: None, vlan: None, style: Style::Inline },
        }
    }

    // The `{host}` path component of the snmpbot URL.
    pub fn path_host(&self) -> String {
        match self.style {
            Style::QueryParam => self.fqdn.clone(),
            Style::Inline => match (&self.community, self.vlan) {
                (Some(c), Some(v)) => format!("{}@{}@{}", c, v, self.fqdn),
                (Some(c), None) => format!("{}@{}", c, self.fqdn),
                (None, _) => self.fqdn.clone(),
            },
        }
    }

    // The `?snmp=` query value, if this style carries the community that way.
    pub fn snmp_query(&self) -> Option<String> {
        match (self.style.clone(), &self.community) {
            (Style::QueryParam, Some(c)) => Some(format!("{}@{}", c, self.fqdn)),
            _ => None,
        }
    }

    // The community string the embedded client passes to the device. For the
    // per-VLAN form this is Cisco community indexing: the literal community is
    // `community@vlan`.
    pub fn effective_community(&self) -> Option<String> {
        let community = self.community.as_ref()?;
        match self.vlan {
            Some(vlan) => Some(format!("{}@{}", community, vlan)),
            None => Some(community.clone()),
        }
    }

    // The HostID echoed back into SNMPBotResultEntry, matching what snmpbot
    // reports for each addressing form (nothing reads it today, but keeping it
    // faithful preserves fixture/mock parity).
    pub fn host_id(&self) -> String {
        self.path_host()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_fqdn() {
        let h = HostSpec::parse("sw1.test.example");
        assert_eq!(h.fqdn, "sw1.test.example");
        assert_eq!(h.community, None);
        assert_eq!(h.vlan, None);
        assert_eq!(h.effective_community(), None);
    }

    #[test]
    fn parse_inline_community() {
        let h = HostSpec::parse("public@sw1.test.example");
        assert_eq!(h.fqdn, "sw1.test.example");
        assert_eq!(h.community.as_deref(), Some("public"));
        assert_eq!(h.effective_community().as_deref(), Some("public"));
        assert_eq!(h.path_host(), "public@sw1.test.example");
        assert_eq!(h.snmp_query(), None);
    }

    #[test]
    fn parse_per_vlan() {
        let h = HostSpec::parse("public@100@sw1.test.example");
        assert_eq!(h.fqdn, "sw1.test.example");
        assert_eq!(h.vlan, Some(100));
        // Cisco community indexing: literal community is community@vlan.
        assert_eq!(h.effective_community().as_deref(), Some("public@100"));
        assert_eq!(h.path_host(), "public@100@sw1.test.example");
    }

    #[test]
    fn query_param_style_reconstructs_url_parts() {
        let h = HostSpec::with_community("sw1.test.example", "public");
        assert_eq!(h.path_host(), "sw1.test.example");
        assert_eq!(h.snmp_query().as_deref(), Some("public@sw1.test.example"));
        assert_eq!(h.effective_community().as_deref(), Some("public"));
    }
}
