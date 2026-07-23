// Per-VLAN spanning-tree computation over the entitypoller's STP data joined
// with the DB link topology (WeathermapBase). Pure functions, no I/O: on every
// non-root bridge exactly one port per VLAN has role "root", pointing toward
// the root bridge; following root ports through the adjacency yields the
// active tree. Ports with role alternate/backUp are the links STP blocks.
use crate::collectors::poller::SNMPBotResultEntryObjectValue;
use crate::models::json::{
    ApiInterfaceConnection, ApiStpBlockedLink, ApiStpBridge, ApiStpNode, ApiStpPort, ApiStpTree,
    WeathermapBase,
};
use std::collections::{BTreeMap, HashMap, HashSet};

// ---------------------------------------------------------------------------
// Value parsing helpers
// ---------------------------------------------------------------------------

// "aa:bb:cc:dd:ee:ff" from any common MAC rendering ("AA BB CC DD EE FF",
// "aa-bb-...", "aabb.ccdd.eeff", already-colon form).
pub fn normalize_mac(mac: &str) -> String {
    let hex: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_lowercase();
    hex.as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<&str>>()
        .join(":")
}

// BRIDGE-MIB BridgeId: 8 octets = 2-byte priority + 6-byte MAC. snmpbot
// renders it as space-separated hex ("81 2c 70 10 6f 63 f2 70" — verified
// against a live C2960CX). Returns (priority, lowercase colon MAC).
pub fn parse_bridge_id(value: &str) -> Option<(i64, String)> {
    let bytes: Vec<u8> = value
        .split_whitespace()
        .map(|tok| u8::from_str_radix(tok, 16))
        .collect::<Result<Vec<u8>, _>>()
        .ok()?;
    if bytes.len() != 8 {
        return None;
    }
    let priority = ((bytes[0] as i64) << 8) | bytes[1] as i64;
    let mac = bytes[2..].iter().map(|b| format!("{:02x}", b)).collect::<Vec<String>>().join(":");
    Some((priority, mac))
}

// snmpbot renders TimeTicks as SECONDS (a Go float64 — integral values
// marshal without a decimal point and land in the Uint64 variant). Verified
// live twice: a C2960CX scalar of 2297973 and an HP RPVST table value of
// 18158911.66 both matched their devices' day-scale topology-change ages.
pub fn timeticks_secs(value: &SNMPBotResultEntryObjectValue) -> Option<i64> {
    match value {
        SNMPBotResultEntryObjectValue::Uint64(v) => Some(*v as i64),
        SNMPBotResultEntryObjectValue::Float64(v) => Some(*v as i64),
        SNMPBotResultEntryObjectValue::Str(s) => s.trim().parse::<f64>().ok().map(|v| v as i64),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tree computation
// ---------------------------------------------------------------------------

pub struct StpInputs<'a> {
    // fqdn -> all STP ports (every vlan; build_stp_tree filters).
    pub ports: &'a HashMap<String, Vec<ApiStpPort>>,
    // fqdn -> per-vlan bridge scalars.
    pub bridges: &'a HashMap<String, Vec<ApiStpBridge>>,
    // fqdn -> dot1dBaseBridgeAddress from discovery (devices.base_mac).
    pub base_macs: &'a HashMap<String, Option<String>>,
    pub topology: &'a WeathermapBase,
    // fqdn -> aggregate ifIndex -> member ifIndexes (LagStore::lag_members).
    // Real switches run STP on the port-channel, which has no LLDP link of
    // its own — adjacency lives on the members.
    pub lag_members: &'a HashMap<String, HashMap<i64, Vec<i64>>>,
}

// The far end of `fqdn`'s interface with SNMP ifIndex `ifindex`, per the DB
// link topology.
fn connected_to(topology: &WeathermapBase, fqdn: &str, ifindex: Option<i64>) -> Option<ApiInterfaceConnection> {
    let ifindex = ifindex?;
    let device = topology.devices.get(fqdn)?;
    let interface = device.interfaces.values().find(|i| i.if_index as i64 == ifindex)?;
    let connection = interface.connected_to.as_ref()?;
    Some(ApiInterfaceConnection { fqdn: connection.fqdn.clone(), interface: connection.interface.clone() })
}

// The far end of an STP port: its own discovered link, or — when the port is
// a LACP aggregate — the first member with one.
fn stp_port_connected_to(inputs: &StpInputs, fqdn: &str, ifindex: Option<i64>) -> Option<ApiInterfaceConnection> {
    if let Some(connection) = connected_to(inputs.topology, fqdn, ifindex) {
        return Some(connection);
    }
    let members = inputs.lag_members.get(fqdn)?.get(&ifindex?)?;
    members.iter().find_map(|member| connected_to(inputs.topology, fqdn, Some(*member)))
}

pub fn build_stp_tree(inputs: &StpInputs, vlan: i64) -> ApiStpTree {
    let mut flags: Vec<String> = Vec::new();

    // Ports on this vlan per device; devices with any are the tree's nodes.
    let mut vlan_ports: BTreeMap<&str, Vec<&ApiStpPort>> = BTreeMap::new();
    for (fqdn, ports) in inputs.ports.iter() {
        let on_vlan: Vec<&ApiStpPort> = ports.iter().filter(|p| p.vlan == vlan).collect();
        if !on_vlan.is_empty() {
            vlan_ports.insert(fqdn.as_str(), on_vlan);
        }
    }
    let node_fqdns: HashSet<&str> = vlan_ports.keys().cloned().collect();

    // Per node: its root port and the resolved upstream (parent) link end.
    struct NodeInfo<'a> {
        root_port: Option<&'a ApiStpPort>,
        parent: Option<ApiInterfaceConnection>, // upstream end; fqdn must be a node
        orphan: bool,
    }
    let mut info: BTreeMap<&str, NodeInfo> = BTreeMap::new();
    for (fqdn, ports) in vlan_ports.iter() {
        let mut root_ports: Vec<&&ApiStpPort> = ports.iter().filter(|p| p.role == "root").collect();
        root_ports.sort_by_key(|p| p.path_cost);
        if root_ports.len() > 1 {
            flags.push(format!("multiple-root-ports:{}", fqdn));
        }
        let root_port = root_ports.into_iter().next().map(|p| *p);
        let (parent, orphan) = match root_port {
            None => (None, false), // root candidate
            Some(port) => {
                let upstream = stp_port_connected_to(inputs, fqdn, port.interface_id);
                match upstream {
                    Some(connection) if node_fqdns.contains(connection.fqdn.as_str()) => (Some(connection), false),
                    // Missing adjacency or upstream device not in the tree.
                    _ => (None, true),
                }
            }
        };
        info.insert(fqdn, NodeInfo { root_port, parent, orphan });
    }

    // Roots: nodes without a root-role port.
    let roots: Vec<String> = info
        .iter()
        .filter(|(_, i)| i.root_port.is_none())
        .map(|(fqdn, _)| fqdn.to_string())
        .collect();
    match roots.len() {
        0 if !info.is_empty() => flags.push("no-root".to_string()),
        0 | 1 => {}
        _ => flags.push("multiple-roots".to_string()),
    }

    // The elected root's identity, used to explain any root mismatch: its
    // base MAC (what reported roots are compared against) and the priority it
    // reports for itself. Only meaningful with a single computed root.
    let computed_root = roots.first().map(|rfqdn| {
        let mac = inputs.base_macs.get(rfqdn).cloned().flatten();
        let priority = inputs
            .bridges
            .get(rfqdn)
            .and_then(|bridges| bridges.iter().find(|b| b.vlan == vlan))
            .and_then(|b| b.root_priority);
        (rfqdn.clone(), mac, priority)
    });
    // Every monitored bridge MAC, so a reported root can be classified as
    // on- or off-fleet.
    let monitored_macs: HashSet<String> = inputs
        .base_macs
        .values()
        .filter_map(|m| m.as_ref())
        .map(|m| normalize_mac(m))
        .collect();

    // Cycle guard: a node whose parent chain loops back on itself is part of
    // a cycle; demote the cycle members (and only them) to orphan. Nodes
    // whose chain merely leads *into* a cycle keep their parent — after the
    // demotion their chain ends at an orphan like any other subtree.
    let mut cycle_nodes: HashSet<String> = HashSet::new();
    for start in info.keys().cloned().collect::<Vec<&str>>() {
        let mut path: Vec<&str> = Vec::new();
        let mut current = start;
        loop {
            if cycle_nodes.contains(current) {
                break; // leads into an already-detected cycle
            }
            if let Some(position) = path.iter().position(|node| *node == current) {
                // Revisit within this walk: the cycle is path[position..];
                // the prefix only leads into it.
                if !flags.contains(&"cycle".to_string()) {
                    flags.push("cycle".to_string());
                }
                cycle_nodes.extend(path[position..].iter().map(|s| s.to_string()));
                break;
            }
            path.push(current);
            match info.get(current).and_then(|i| i.parent.as_ref()) {
                Some(parent) => match info.keys().find(|k| **k == parent.fqdn.as_str()) {
                    Some(parent_key) => current = parent_key,
                    None => break,
                },
                None => break,
            }
        }
    }
    for fqdn in cycle_nodes.iter() {
        if let Some(node) = info.get_mut(fqdn.as_str()) {
            node.parent = None;
            node.orphan = true;
        }
    }

    // Children map for DFS emission: real roots first, then orphan subtrees.
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (fqdn, node) in info.iter() {
        if let Some(parent) = node.parent.as_ref() {
            if let Some((parent_key, _)) = info.iter().find(|(k, _)| **k == parent.fqdn.as_str()) {
                children.entry(parent_key).or_default().push(fqdn);
            }
        }
    }

    let mut nodes: Vec<ApiStpNode> = Vec::new();
    let mut emitted: HashSet<&str> = HashSet::new();
    let starts: Vec<&str> = info
        .iter()
        .filter(|(_, i)| i.root_port.is_none() || i.orphan)
        .map(|(fqdn, _)| *fqdn)
        .collect();
    for start in starts {
        // DFS with explicit stack; children already sorted (BTreeMap order).
        let mut stack: Vec<(&str, i64)> = vec![(start, 0)];
        while let Some((fqdn, depth)) = stack.pop() {
            if !emitted.insert(fqdn) {
                continue;
            }
            let node = &info[fqdn];
            let reported = inputs
                .bridges
                .get(fqdn)
                .and_then(|bridges| bridges.iter().find(|b| b.vlan == vlan))
                .cloned();
            // Mismatch only checkable with a single computed root whose
            // bridge MAC discovery recorded.
            let root_mismatch = match (roots.len(), reported.as_ref().and_then(|r| r.root_mac.clone())) {
                (1, Some(reported_mac)) => match inputs.base_macs.get(&roots[0]).cloned().flatten() {
                    Some(root_mac) => normalize_mac(&reported_mac) != normalize_mac(&root_mac),
                    None => false,
                },
                _ => false,
            };
            // When mismatched, capture both sides for the Issues detail view:
            // who jaspy elected, what this node reports, and — decisively —
            // which bridge ID STP would actually prefer.
            let root_mismatch_detail = if root_mismatch {
                computed_root.as_ref().map(|(root_fqdn, root_mac, root_priority)| {
                    let reported_mac = reported.as_ref().and_then(|b| b.root_mac.clone());
                    let reported_priority = reported.as_ref().and_then(|b| b.root_priority);
                    let reported_root_monitored = reported_mac
                        .as_ref()
                        .map(|m| monitored_macs.contains(&normalize_mac(m)))
                        .unwrap_or(false);
                    // Lower bridge ID wins: compare priority first, MAC on a tie.
                    let reported_root_superior = match (reported_priority, root_priority) {
                        (Some(rp), Some(cp)) if rp != *cp => Some(rp < *cp),
                        (Some(_), Some(_)) => match (reported_mac.as_ref(), root_mac.as_ref()) {
                            (Some(a), Some(b)) => Some(normalize_mac(a) < normalize_mac(b)),
                            _ => None,
                        },
                        _ => None,
                    };
                    crate::models::json::ApiStpRootMismatchDetail {
                        computed_root_fqdn: root_fqdn.clone(),
                        computed_root_hostname: root_fqdn.split('.').next().unwrap_or(root_fqdn).to_string(),
                        // Normalize both sides to lowercase colon form so the UI
                        // compares/displays them consistently regardless of how
                        // each device rendered its bridge ID.
                        computed_root_mac: root_mac.as_ref().map(|m| normalize_mac(m)),
                        computed_root_priority: *root_priority,
                        reported_root_mac: reported_mac.as_ref().map(|m| normalize_mac(m)),
                        reported_root_priority: reported_priority,
                        reported_root_monitored,
                        reported_root_superior,
                    }
                })
            } else {
                None
            };
            nodes.push(ApiStpNode {
                fqdn: fqdn.to_string(),
                depth,
                parent: node.parent.as_ref().map(|p| p.fqdn.clone()),
                parent_interface: node.parent.as_ref().map(|p| p.interface.clone()),
                root_port_interface_name: node.root_port.and_then(|p| p.interface_name.clone()),
                root_port_state: node.root_port.map(|p| p.state.clone()),
                path_cost: node.root_port.map(|p| p.path_cost),
                reported,
                root_mismatch,
                root_mismatch_detail,
                orphan: node.orphan,
            });
            // Push children reversed so the DFS emits them in sorted order.
            for child in children.get(fqdn).into_iter().flatten().rev() {
                stack.push((child, depth + 1));
            }
        }
    }

    // Blocked links: alternate/backUp ports with their adjacency-resolved end.
    let mut blocked_links: Vec<ApiStpBlockedLink> = Vec::new();
    for (fqdn, ports) in vlan_ports.iter() {
        for port in ports.iter().filter(|p| p.role == "alternate" || p.role == "backUp") {
            blocked_links.push(ApiStpBlockedLink {
                fqdn: fqdn.to_string(),
                stp_port_id: port.stp_port_id,
                interface_name: port.interface_name.clone(),
                role: port.role.clone(),
                state: port.state.clone(),
                path_cost: port.path_cost,
                connected_to: stp_port_connected_to(inputs, fqdn, port.interface_id),
            });
        }
    }

    ApiStpTree { vlan, roots, nodes, blocked_links, flags }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::json::{WeathermapDevice, WeathermapDeviceInterface, WeathermapDeviceInterfaceConnectedTo};

    fn port(vlan: i64, role: &str, state: &str, ifindex: i64, ifname: &str, cost: i64) -> ApiStpPort {
        ApiStpPort {
            vlan,
            stp_port_id: ifindex % 100,
            interface_name: Some(ifname.to_string()),
            interface_id: Some(ifindex),
            role: role.to_string(),
            state: state.to_string(),
            enabled: Some(true),
            designated_cost: 0,
            path_cost: cost,
            priority: 128,
            forward_transitions: 1,
            timestamp: 1,
        }
    }

    fn bridge(vlan: i64, mac: &str, cost: i64) -> ApiStpBridge {
        bridge_prio(vlan, mac, 32768 + vlan, cost)
    }

    // A reported bridge with an explicit root priority — for mismatch cases
    // that hinge on which bridge ID STP would prefer.
    fn bridge_prio(vlan: i64, mac: &str, priority: i64, cost: i64) -> ApiStpBridge {
        ApiStpBridge {
            vlan,
            root_priority: Some(priority),
            root_mac: Some(mac.to_string()),
            root_cost: Some(cost),
            root_port: None,
            root_port_interface_name: None,
            topology_changes: Some(3),
            time_since_topology_change_secs: Some(600),
            timestamp: 1,
        }
    }

    // Topology helper: device with interfaces [(ifindex, name, peer)].
    fn topo_device(fqdn: &str, interfaces: &[(i32, &str, Option<(&str, &str)>)]) -> (String, WeathermapDevice) {
        let mut map = HashMap::new();
        for (if_index, name, peer) in interfaces {
            map.insert(name.to_string(), WeathermapDeviceInterface {
                name: name.to_string(),
                if_index: *if_index,
                connected_to: peer.map(|(fqdn, interface)| WeathermapDeviceInterfaceConnectedTo {
                    fqdn: fqdn.to_string(),
                    interface: interface.to_string(),
                }),
            });
        }
        (fqdn.to_string(), WeathermapDevice { fqdn: fqdn.to_string(), interfaces: map })
    }

    // core -> dist -> (leaf-a, leaf-b); leaf-b also has a blocked alternate
    // port toward core.
    fn three_level_inputs() -> (HashMap<String, Vec<ApiStpPort>>, HashMap<String, Vec<ApiStpBridge>>, HashMap<String, Option<String>>, WeathermapBase) {
        let mut ports = HashMap::new();
        ports.insert("core.x".to_string(), vec![port(10, "designated", "forwarding", 101, "c1", 0)]);
        ports.insert("dist.x".to_string(), vec![
            port(10, "root", "forwarding", 201, "d-up", 4),
            port(10, "designated", "forwarding", 202, "d-down-a", 0),
            port(10, "designated", "forwarding", 203, "d-down-b", 0),
        ]);
        ports.insert("leaf-a.x".to_string(), vec![port(10, "root", "forwarding", 301, "a-up", 8)]);
        ports.insert("leaf-b.x".to_string(), vec![
            port(10, "root", "forwarding", 401, "b-up", 8),
            port(10, "alternate", "blocking", 402, "b-alt", 19),
        ]);

        let mut bridges = HashMap::new();
        for fqdn in ["core.x", "dist.x", "leaf-a.x", "leaf-b.x"] {
            bridges.insert(fqdn.to_string(), vec![bridge(10, "02:00:00:00:10:01", 0)]);
        }

        let mut base_macs = HashMap::new();
        base_macs.insert("core.x".to_string(), Some("02:00:00:00:10:01".to_string()));
        base_macs.insert("dist.x".to_string(), Some("02:00:00:00:10:02".to_string()));
        base_macs.insert("leaf-a.x".to_string(), Some("02:00:00:00:10:03".to_string()));
        base_macs.insert("leaf-b.x".to_string(), Some("02:00:00:00:10:04".to_string()));

        let mut devices = HashMap::new();
        for (fqdn, device) in [
            topo_device("core.x", &[(101, "c1", Some(("dist.x", "d-up"))), (102, "c2", Some(("leaf-b.x", "b-alt")))]),
            topo_device("dist.x", &[
                (201, "d-up", Some(("core.x", "c1"))),
                (202, "d-down-a", Some(("leaf-a.x", "a-up"))),
                (203, "d-down-b", Some(("leaf-b.x", "b-up"))),
            ]),
            topo_device("leaf-a.x", &[(301, "a-up", Some(("dist.x", "d-down-a")))]),
            topo_device("leaf-b.x", &[
                (401, "b-up", Some(("dist.x", "d-down-b"))),
                (402, "b-alt", Some(("core.x", "c2"))),
            ]),
        ] {
            devices.insert(fqdn, device);
        }
        (ports, bridges, base_macs, WeathermapBase { devices })
    }

    #[test]
    fn aggregate_root_port_resolves_parent_via_lag_members() {
        // dist's root port is Po1 (ifindex 5001): no link of its own, but its
        // members 201/205 are the discovered links to core. Without lag data
        // the node orphans; with it the parent resolves through a member.
        let mut ports = HashMap::new();
        ports.insert("core.x".to_string(), vec![port(10, "designated", "forwarding", 5001, "Po1", 0)]);
        ports.insert("dist.x".to_string(), vec![port(10, "root", "forwarding", 5001, "Po1", 3)]);
        let mut bridges = HashMap::new();
        for fqdn in ["core.x", "dist.x"] {
            bridges.insert(fqdn.to_string(), vec![bridge(10, "02:00:00:00:10:01", 0)]);
        }
        let mut base_macs = HashMap::new();
        base_macs.insert("core.x".to_string(), Some("02:00:00:00:10:01".to_string()));
        base_macs.insert("dist.x".to_string(), Some("02:00:00:00:10:02".to_string()));
        let mut devices = HashMap::new();
        for (fqdn, device) in [
            topo_device("core.x", &[(101, "c1", Some(("dist.x", "d-up"))), (105, "c5", Some(("dist.x", "d-up2")))]),
            topo_device("dist.x", &[
                (201, "d-up", Some(("core.x", "c1"))),
                (205, "d-up2", Some(("core.x", "c5"))),
            ]),
        ] {
            devices.insert(fqdn, device);
        }
        let topology = WeathermapBase { devices };

        let orphaned = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let dist = orphaned.nodes.iter().find(|n| n.fqdn == "dist.x").unwrap();
        assert!(dist.orphan, "without lag membership the Po root port cannot resolve");

        let mut lag_members = HashMap::new();
        lag_members.insert("dist.x".to_string(), HashMap::from([(5001i64, vec![201i64, 205])]));
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &lag_members }, 10);
        let dist = tree.nodes.iter().find(|n| n.fqdn == "dist.x").unwrap();
        assert!(!dist.orphan);
        assert_eq!(dist.parent.as_deref(), Some("core.x"));
        assert_eq!(dist.depth, 1);
        assert_eq!(dist.root_port_interface_name.as_deref(), Some("Po1"));
    }

    #[test]
    fn three_level_tree_dfs_order_depths_and_links() {
        let (ports, bridges, base_macs, topology) = three_level_inputs();
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);

        assert_eq!(tree.roots, vec!["core.x"]);
        assert!(tree.flags.is_empty(), "flags: {:?}", tree.flags);
        let order: Vec<(&str, i64)> = tree.nodes.iter().map(|n| (n.fqdn.as_str(), n.depth)).collect();
        assert_eq!(order, vec![("core.x", 0), ("dist.x", 1), ("leaf-a.x", 2), ("leaf-b.x", 2)]);

        let dist = &tree.nodes[1];
        assert_eq!(dist.parent.as_deref(), Some("core.x"));
        assert_eq!(dist.parent_interface.as_deref(), Some("c1"));
        assert_eq!(dist.root_port_interface_name.as_deref(), Some("d-up"));
        assert_eq!(dist.root_port_state.as_deref(), Some("forwarding"));
        assert_eq!(dist.path_cost, Some(4));
        assert!(!dist.orphan);
        assert!(!dist.root_mismatch);
        assert!(dist.reported.is_some());

        let core = &tree.nodes[0];
        assert_eq!(core.parent, None);
        assert_eq!(core.path_cost, None);
    }

    #[test]
    fn blocked_links_resolve_far_end() {
        let (ports, bridges, base_macs, topology) = three_level_inputs();
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        assert_eq!(tree.blocked_links.len(), 1);
        let blocked = &tree.blocked_links[0];
        assert_eq!(blocked.fqdn, "leaf-b.x");
        assert_eq!(blocked.role, "alternate");
        assert_eq!(blocked.state, "blocking");
        let far = blocked.connected_to.as_ref().unwrap();
        assert_eq!((far.fqdn.as_str(), far.interface.as_str()), ("core.x", "c2"));
    }

    #[test]
    fn missing_adjacency_makes_orphan_subtree() {
        let (mut ports, bridges, base_macs, mut topology) = three_level_inputs();
        // leaf-a's uplink loses its adjacency row.
        topology.devices.get_mut("leaf-a.x").unwrap().interfaces.get_mut("a-up").unwrap().connected_to = None;
        // give leaf-a a child to prove the orphan subtree keeps its shape
        ports.insert("leaf-a2.x".to_string(), vec![port(10, "root", "forwarding", 501, "a2-up", 12)]);
        let (fqdn, device) = topo_device("leaf-a2.x", &[(501, "a2-up", Some(("leaf-a.x", "a-down")))]);
        topology.devices.insert(fqdn, device);
        topology.devices.get_mut("leaf-a.x").unwrap().interfaces.insert("a-down".to_string(), WeathermapDeviceInterface {
            name: "a-down".to_string(), if_index: 302,
            connected_to: Some(WeathermapDeviceInterfaceConnectedTo { fqdn: "leaf-a2.x".to_string(), interface: "a2-up".to_string() }),
        });

        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let leaf_a = tree.nodes.iter().find(|n| n.fqdn == "leaf-a.x").unwrap();
        assert!(leaf_a.orphan);
        assert_eq!(leaf_a.parent, None);
        assert_eq!(leaf_a.depth, 0, "orphan starts its own subtree");
        let leaf_a2 = tree.nodes.iter().find(|n| n.fqdn == "leaf-a2.x").unwrap();
        assert_eq!(leaf_a2.parent.as_deref(), Some("leaf-a.x"));
        assert_eq!(leaf_a2.depth, 1);
        // The main tree is unaffected.
        assert_eq!(tree.roots, vec!["core.x"]);
    }

    #[test]
    fn unmonitored_parent_makes_orphan() {
        let (mut ports, bridges, base_macs, topology) = three_level_inputs();
        // Remove the core from the STP data (e.g. unmonitored root device):
        // dist's root port now points at a non-node.
        ports.remove("core.x");
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let dist = tree.nodes.iter().find(|n| n.fqdn == "dist.x").unwrap();
        assert!(dist.orphan);
        // Every remaining node has a root port, so no root candidate exists.
        assert!(tree.roots.is_empty());
        assert!(tree.flags.contains(&"no-root".to_string()));
    }

    #[test]
    fn multiple_roots_flagged() {
        let (mut ports, bridges, base_macs, topology) = three_level_inputs();
        // leaf-b claims rootness too (no root port).
        ports.insert("leaf-b.x".to_string(), vec![port(10, "designated", "forwarding", 401, "b-up", 0)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        assert_eq!(tree.roots, vec!["core.x", "leaf-b.x"]);
        assert!(tree.flags.contains(&"multiple-roots".to_string()));
    }

    #[test]
    fn multiple_roots_suppresses_root_mismatch() {
        // With more than one computed root there is no single reference to
        // compare against, so no node is flagged as disagreeing — the
        // "multiple-roots" flag already tells the story. (Guards against
        // double-alerting a split tree as N root mismatches.)
        let (mut ports, mut bridges, base_macs, topology) = three_level_inputs();
        ports.insert("leaf-b.x".to_string(), vec![port(10, "designated", "forwarding", 401, "b-up", 0)]);
        // leaf-b even reports a different root — still must not be a mismatch.
        bridges.insert("leaf-b.x".to_string(), vec![bridge(10, "02 00 00 00 99 99", 0)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        assert!(tree.flags.contains(&"multiple-roots".to_string()));
        assert!(tree.nodes.iter().all(|n| !n.root_mismatch));
        assert!(tree.nodes.iter().all(|n| n.root_mismatch_detail.is_none()));
    }

    #[test]
    fn root_mismatch_against_monitored_superior_bridge() {
        // leaf-b reports a *monitored* peer (dist.x) as root, with a better
        // (lower) priority than the elected root — a genuine split-brain
        // between two monitored bridges, not an off-fleet upstream.
        let (ports, mut bridges, base_macs, topology) = three_level_inputs();
        bridges.insert("leaf-b.x".to_string(), vec![bridge_prio(10, "02:00:00:00:10:02", 4096, 8)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let detail = tree.nodes.iter().find(|n| n.fqdn == "leaf-b.x").unwrap().root_mismatch_detail.as_ref().unwrap();
        assert_eq!(detail.computed_root_fqdn, "core.x");
        assert!(detail.reported_root_monitored); // dist.x is a fleet device
        assert_eq!(detail.reported_root_superior, Some(true)); // 4096 < 32778
    }

    #[test]
    fn root_mismatch_with_weaker_reported_root_is_not_superior() {
        // leaf-b reports an off-fleet root with a *worse* (higher) priority —
        // its STP view is stale/partitioned, the elected root still wins.
        let (ports, mut bridges, base_macs, topology) = three_level_inputs();
        bridges.insert("leaf-b.x".to_string(), vec![bridge_prio(10, "02 00 00 00 99 99", 61440, 8)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let detail = tree.nodes.iter().find(|n| n.fqdn == "leaf-b.x").unwrap().root_mismatch_detail.as_ref().unwrap();
        assert!(!detail.reported_root_monitored);
        assert_eq!(detail.reported_root_superior, Some(false)); // 61440 > 32778
    }

    #[test]
    fn root_mismatch_superiority_breaks_ties_on_mac() {
        // Equal priority → STP decides on the lower MAC. The elected root
        // (core.x) has base MAC 02:00:00:00:10:01 and reports priority 32778.
        let base = 32768 + 10;
        // Lower reported MAC wins the tie → reported root is superior.
        let (ports, mut bridges, base_macs, topology) = three_level_inputs();
        bridges.insert("leaf-b.x".to_string(), vec![bridge_prio(10, "00:00:00:00:00:01", base, 8)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let lower = tree.nodes.iter().find(|n| n.fqdn == "leaf-b.x").unwrap().root_mismatch_detail.as_ref().unwrap();
        assert_eq!(lower.reported_root_superior, Some(true));

        // Higher reported MAC loses the tie → not superior.
        let (ports, mut bridges, base_macs, topology) = three_level_inputs();
        bridges.insert("leaf-b.x".to_string(), vec![bridge_prio(10, "ff:ff:ff:ff:ff:ff", base, 8)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let higher = tree.nodes.iter().find(|n| n.fqdn == "leaf-b.x").unwrap().root_mismatch_detail.as_ref().unwrap();
        assert_eq!(higher.reported_root_superior, Some(false));
    }

    #[test]
    fn root_mismatch_needs_a_known_computed_root_mac() {
        // The elected root's base MAC is unknown (discovery never recorded it),
        // so there is nothing to compare against — no mismatch is raised.
        let (ports, mut bridges, mut base_macs, topology) = three_level_inputs();
        base_macs.insert("core.x".to_string(), None);
        bridges.insert("leaf-b.x".to_string(), vec![bridge(10, "02 00 00 00 99 99", 8)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        assert!(tree.nodes.iter().all(|n| !n.root_mismatch));
    }

    #[test]
    fn cycle_demotes_to_orphans_and_flags() {
        let (mut ports, bridges, base_macs, mut topology) = three_level_inputs();
        // dist and core point at each other with root ports (data anomaly).
        ports.insert("core.x".to_string(), vec![port(10, "root", "forwarding", 101, "c1", 4)]);
        topology.devices.get_mut("core.x").unwrap().interfaces.get_mut("c1").unwrap().connected_to =
            Some(WeathermapDeviceInterfaceConnectedTo { fqdn: "dist.x".to_string(), interface: "d-up".to_string() });
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        assert!(tree.flags.contains(&"cycle".to_string()), "flags: {:?}", tree.flags);
        let core = tree.nodes.iter().find(|n| n.fqdn == "core.x").unwrap();
        let dist = tree.nodes.iter().find(|n| n.fqdn == "dist.x").unwrap();
        assert!(core.orphan && dist.orphan);
        // The leaves only lead *into* the cycle: they keep their parent and
        // hang off the demoted dist rather than being orphaned themselves.
        for leaf in ["leaf-a.x", "leaf-b.x"] {
            let node = tree.nodes.iter().find(|n| n.fqdn == leaf).unwrap();
            assert!(!node.orphan, "{} should not be demoted", leaf);
            assert_eq!(node.parent.as_deref(), Some("dist.x"));
        }
        // Every node still appears exactly once.
        assert_eq!(tree.nodes.len(), 4);
    }

    #[test]
    fn root_mismatch_detected_across_mac_formats() {
        let (ports, mut bridges, base_macs, topology) = three_level_inputs();
        // leaf-b reports a different root, in a different MAC rendering.
        bridges.insert("leaf-b.x".to_string(), vec![bridge(10, "02 00 00 00 99 99", 8)]);
        // leaf-a agrees with the computed root, spaced-uppercase form.
        bridges.insert("leaf-a.x".to_string(), vec![bridge(10, "02 00 00 00 10 01", 8)]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let leaf_b = tree.nodes.iter().find(|n| n.fqdn == "leaf-b.x").unwrap();
        assert!(leaf_b.root_mismatch);
        assert!(!tree.nodes.iter().find(|n| n.fqdn == "leaf-a.x").unwrap().root_mismatch);

        // The mismatch carries both sides: jaspy's elected root (core.x) and
        // the off-fleet MAC leaf-b reports.
        let detail = leaf_b.root_mismatch_detail.as_ref().expect("mismatch detail present");
        assert_eq!(detail.computed_root_fqdn, "core.x");
        assert_eq!(detail.computed_root_mac.as_deref(), Some("02:00:00:00:10:01"));
        assert_eq!(detail.reported_root_mac.as_deref(), Some("02:00:00:00:99:99"));
        // 99:99 is not a monitored base MAC.
        assert!(!detail.reported_root_monitored);
        // leaf-a, which agrees, has no mismatch detail.
        assert!(tree.nodes.iter().find(|n| n.fqdn == "leaf-a.x").unwrap().root_mismatch_detail.is_none());
    }

    #[test]
    fn multiple_root_ports_take_lowest_cost_and_flag() {
        let (mut ports, bridges, base_macs, topology) = three_level_inputs();
        ports.insert("leaf-b.x".to_string(), vec![
            port(10, "root", "forwarding", 401, "b-up", 8),
            port(10, "root", "forwarding", 402, "b-alt", 19),
        ]);
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 10);
        let leaf_b = tree.nodes.iter().find(|n| n.fqdn == "leaf-b.x").unwrap();
        assert_eq!(leaf_b.root_port_interface_name.as_deref(), Some("b-up"));
        assert!(tree.flags.contains(&"multiple-root-ports:leaf-b.x".to_string()));
    }

    #[test]
    fn vlan_filtering_excludes_other_vlans() {
        let (ports, bridges, base_macs, topology) = three_level_inputs();
        let tree = build_stp_tree(&StpInputs { ports: &ports, bridges: &bridges, base_macs: &base_macs, topology: &topology, lag_members: &HashMap::new() }, 20);
        assert!(tree.nodes.is_empty());
        assert!(tree.roots.is_empty());
        assert!(tree.flags.is_empty(), "empty vlan is not an anomaly");
    }

    // --- value parsing ---

    #[test]
    fn bridge_id_parses_live_format() {
        // Real value from a C2960CX: priority 0x812c = 33068, mac 70:10:6f:63:f2:70.
        let (priority, mac) = parse_bridge_id("81 2c 70 10 6f 63 f2 70").unwrap();
        assert_eq!(priority, 33068);
        assert_eq!(mac, "70:10:6f:63:f2:70");
    }

    #[test]
    fn bridge_id_rejects_wrong_length_and_garbage() {
        assert!(parse_bridge_id("81 2c 70 10 6f 63 f2").is_none());
        assert!(parse_bridge_id("81 2c 70 10 6f 63 f2 70 00").is_none());
        assert!(parse_bridge_id("81 zz 70 10 6f 63 f2 70").is_none());
        assert!(parse_bridge_id("").is_none());
    }

    #[test]
    fn normalize_mac_handles_common_forms() {
        for form in ["70:10:6F:63:F2:70", "70 10 6f 63 f2 70", "70-10-6f-63-f2-70", "7010.6f63.f270"] {
            assert_eq!(normalize_mac(form), "70:10:6f:63:f2:70", "form: {}", form);
        }
    }

    #[test]
    fn timeticks_are_snmpbot_seconds() {
        // Live C2960CX scalar: 2297973 s ≈ 26.6 days (integral floats lose
        // their decimal point in Go's JSON and parse as Uint64).
        assert_eq!(timeticks_secs(&SNMPBotResultEntryObjectValue::Uint64(2297973)), Some(2297973));
        // Live HP RPVST table value: 18158911.66 s ≈ 210 days.
        assert_eq!(timeticks_secs(&SNMPBotResultEntryObjectValue::Float64(18158911.66)), Some(18158911));
        assert_eq!(timeticks_secs(&SNMPBotResultEntryObjectValue::Str("500.5".to_string())), Some(500));
        assert_eq!(timeticks_secs(&SNMPBotResultEntryObjectValue::Str("1h2m".to_string())), None);
        assert_eq!(timeticks_secs(&SNMPBotResultEntryObjectValue::Empty), None);
    }
}
