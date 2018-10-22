import time
import logging
import argparse
import threading
import socket
from lib.SNMPDataSource import SNMPDataSource
import json
import requests


logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)-15s %(levelname)-8s %(name)-12s %(message)s'
)
logger = logging.getLogger('discover')

parser = argparse.ArgumentParser(description='perform jaspy network discovery')
parser.add_argument('-s', '--snmpbot-url', required=False, help='url for snmpbot', default='http://localhost:8286')
parser.add_argument('-j', '--jaspy-url', required=False, help='url for jaspy', default='http://localhost:8000')
parser.add_argument('-c', '--community', required=True, help='snmp community')
parser.add_argument('-r', '--root-device', required=True, help='root device for discovery')
parser.add_argument('-d', '--dns-domain', required=True, help='dns domains', action='append', default=None)
parser.add_argument('-D', '--debug', required=False, action='store_const', const=True, default=False)
parser.add_argument('-S', '--stable', required=False, action='store_const', const=True, default=False)
args = parser.parse_args()

if args.debug:
    logger.setLevel(logging.DEBUG)
else:
    logging.getLogger('urllib3.connectionpool').setLevel(logging.WARN)


def try_resolve(device_name):
    if device_name is None:
        return None
    if device_name.strip() == '':
        return None
    hndn = device_name.split('.', 1)
    try:
        hn = hndn[0]
        if len(hndn) == 2:
            dn = hndn[1]
            fqdn = '%s.%s.' % (hn, dn)
            socket.gethostbyname(fqdn)
            return '%s.%s' % (hn, dn)
        else:
            for search_domain in args.dns_domain:
                try:
                    fqdn = '%s.%s.' % (hn, search_domain)
                    socket.gethostbyname(fqdn)
                    return '%s.%s' % (hn, search_domain)
                except socket.gaierror as gaie:
                    pass
            logger.info('failed to resolve %s using any search domain', device_name)
            return None
    except socket.gaierror:
        logger.info('failed to resolve fqdn %s', device_name)
        return None


def get_remote_name(interface_info, protocol, protocol_field):
    if protocol not in interface_info['_neighbors']:
        return None
    protocol_info = interface_info['_neighbors'][protocol]
    if protocol_field not in protocol_info or protocol_info[protocol_field] is None:
        return None
    nbr_info = protocol_info[protocol_field].strip()
    if len(nbr_info) == 0:
        return None
    fqdn = try_resolve(nbr_info)
    return fqdn

def send_device_info(device):
    device_name, device_domain = device.device_fqdn.split('.', 1)
    device_json = {
        'name': device_name,
        'deviceType': device.device_type(),
        'dnsDomain': device_domain,
        'snmpCommunity': device.community(),
        'baseMac': device.get_chassis_id(),
        'osInfo': device.os_info()
    }
    interfaces_json = {}

    def value_or_none(i, k):
        if k in i and len(k.strip()) > 0:
            return i[k]
        return None

    for interface in device.interfaces.values():
        interface_info = {
            'index': interface['IF-MIB::ifIndex'],
            'interfaceType': interface['IF-MIB::ifType'],
            'name': interface['IF-MIB::ifName'],
            'alias': value_or_none(interface, 'IF-MIB::ifAlias'),
            'description': value_or_none(interface, 'IF-MIB::ifDescr')
        }
        interfaces_json[interface_info['name']] = interface_info

    device_json['interfaces'] = interfaces_json

    requests.put('%s/discovery/device' % (args.jaspy_url), json=device_json)


def discover_device(device_fqdn, detected_devices, detector_threads, detector_lock):
    sds = SNMPDataSource(device_fqdn, args.snmpbot_url, args.community)
    try:
        sds.collect()
    except BaseException as be:
        logger.error('failed to discover %s', device_fqdn)
        logger.exception(be)
    with detector_lock:
        detected_devices[device_fqdn] = sds
    tmp_discovered_neighbors = []
    for key, value in sds.interfaces.items():
        if '_neighbors' in value:
            lldp_neighbor = get_remote_name(value, 'lldp', 'LLDP-MIB::lldpRemSysName')
            if lldp_neighbor is not None and lldp_neighbor not in tmp_discovered_neighbors:
                tmp_discovered_neighbors.append(lldp_neighbor)
            cdp_neighbor = get_remote_name(value, 'cdp', 'CISCO-CDP-MIB::cdpCacheDeviceId')
            if cdp_neighbor is not None and cdp_neighbor not in tmp_discovered_neighbors:
                tmp_discovered_neighbors.append(cdp_neighbor)
    with detector_lock:
        for tmp_discovered_neighbor in tmp_discovered_neighbors:
            if tmp_discovered_neighbor in detected_devices or tmp_discovered_neighbor in detector_threads:
                continue
            start_device_discovery(tmp_discovered_neighbor, detected_devices, detector_threads, detector_lock)
    send_device_info(sds)


def start_device_discovery(device_fqdn, detected_devices, detector_threads, detector_lock):
    detector_threads[device_fqdn] = threading.Thread(
        target=discover_device,
        args=(device_fqdn, detected_devices, detector_threads, detector_lock)
    )
    detector_threads[device_fqdn].start()
    logger.debug('-> %s', device_fqdn)


def perform_discovery(root_device, detected_devices):
    detector_threads = {}
    detector_lock = threading.Lock()
    start_device_discovery(root_device, detected_devices, detector_threads, detector_lock)
    while len(detector_threads) > 0:
        with detector_lock:
            joinable_threads = []
            for fqdn, detector_thread in detector_threads.items():
                if not detector_thread.is_alive():
                    joinable_threads.append(fqdn)
            for joinable_thread in joinable_threads:
                detector_threads[joinable_thread].join()
                del detector_threads[joinable_thread]
                logger.debug('<- %s', joinable_thread)
        logger.info('discovery in progress for: %s', list(detector_threads.keys()))
        time.sleep(1)


def lookup_lldp_neighbor(detected_devices, lldp_neighbor_descriptor):
    fqdn = try_resolve(lldp_neighbor_descriptor['LLDP-MIB::lldpRemSysName'])
    device = None
    if fqdn not in detected_devices:
        colond_chassis_id = lldp_neighbor_descriptor['LLDP-MIB::lldpRemChassisId'].replace(' ', ':')
        for detected_device in detected_devices.values():
            if detected_device.get_chassis_id() == colond_chassis_id:
                device = detected_device
                break
    else:
        device = detected_devices[fqdn]
    return device


def lookup_lldp_neighbor_port(
        detected_devices, local_device, local_port, lldp_neighbor_descriptor, lldp_neighbor, is_reverse=False):
    # direct lookup, this is the most reliable one in terms of getting the correct result
    remote_port = lldp_neighbor.lookup_port_by_lldp_remote_info(
        lldp_neighbor_descriptor['LLDP-MIB::lldpRemPortId'],
        lldp_neighbor_descriptor['LLDP-MIB::lldpRemPortIdSubtype']
    )
    if remote_port is not None:
        return remote_port
    if not is_reverse:
        num_refs = 0
        last_checked_interface = None
        for rev_interface in lldp_neighbor.interfaces.values():
            if 'lldp' in rev_interface['_neighbors']:
                rev_lldp_neighbor_descriptor = rev_interface['_neighbors']['lldp']
                rev_lldp_neighbor = lookup_lldp_neighbor(detected_devices, rev_lldp_neighbor_descriptor)
                if rev_lldp_neighbor == local_device:
                    num_refs += 1
                    last_checked_interface = rev_interface
                    rev_lldp_neighbor_port = lookup_lldp_neighbor_port(
                        detected_devices, lldp_neighbor, rev_interface,
                        rev_lldp_neighbor_descriptor, rev_lldp_neighbor, True
                    )
                    if rev_lldp_neighbor_port == local_port:
                        return rev_interface
        if num_refs == 1 and last_checked_interface is not None:
            return last_checked_interface
    logger.error(
        '[LLDP] giving up on %s:%s (%s)',
        local_device.device_fqdn, local_port['IF-MIB::ifName'], lldp_neighbor.device_fqdn
    )


def lookup_cdp_neighbor(detected_devices, cdp_neighbor_descriptor):
    fqdn = try_resolve(cdp_neighbor_descriptor['CISCO-CDP-MIB::cdpCacheDeviceId'])
    if fqdn in detected_devices:
        return detected_devices[fqdn]
    else:
        return None


def lookup_cdp_neighbor_port(
        detected_devices, local_device, local_port, cdp_neighbor_descriptor, cdp_neighbor, is_reverse=False):
    # note: reverse lookup not implemented but kept in function case of need
    remote_port = cdp_neighbor.lookup_port_by_cdp_info(
        cdp_neighbor_descriptor['CISCO-CDP-MIB::cdpCacheDevicePort']
    )
    if remote_port is not None:
        return remote_port
    logger.error(
        '[CDP] giving up on %s:%s (%s)',
        local_device.device_fqdn, local_port['IF-MIB::ifName'], cdp_neighbor.device_fqdn
    )


def build_connections(detected_devices):
    for fqdn, device in detected_devices.items():
        for ifindex, interface in device.interfaces.items():
            if len(interface['_neighbors']) == 0:
                continue
            interface['_link'] = None
            cdp_link_candidate = None
            if 'cdp' in interface['_neighbors']:
                cdp_neighbor_descriptor = interface['_neighbors']['cdp']
                cdp_neighbor = lookup_cdp_neighbor(detected_devices, cdp_neighbor_descriptor)
                if cdp_neighbor is not None:
                    cdp_neighbor_port = lookup_cdp_neighbor_port(
                        detected_devices, device, interface, cdp_neighbor_descriptor, cdp_neighbor
                    )
                    if cdp_neighbor_port is not None:
                        cdp_link_candidate = (cdp_neighbor, cdp_neighbor_port)
            lldp_link_candidate = None
            if 'lldp' in interface['_neighbors']:
                lldp_neighbor_descriptor = interface['_neighbors']['lldp']
                lldp_neighbor = lookup_lldp_neighbor(detected_devices, lldp_neighbor_descriptor)
                if lldp_neighbor is not None:
                    lldp_neighbor_port = lookup_lldp_neighbor_port(
                        detected_devices, device, interface, lldp_neighbor_descriptor, lldp_neighbor
                    )
                    if lldp_neighbor_port is not None:
                        if interface['_link'] is None:
                            lldp_link_candidate = (lldp_neighbor, lldp_neighbor_port)
            if lldp_link_candidate is not None:
                interface['_link'] = lldp_link_candidate
            else:
                interface['_link'] = cdp_link_candidate
            if interface['_link'] is not None:
                l_device, l_port = interface['_link']
                logger.info(
                    'LINK %s:%s -> %s:%s',
                    fqdn, interface['IF-MIB::ifName'], l_device.device_fqdn, l_port['IF-MIB::ifName']
                )


def send_device_topology_info(device):
    device_link_info = {'interfaces': {}}
    for interface in device.interfaces.values():
        if '_link' not in interface or interface['_link'] is None:
            device_link_info['interfaces'][interface['IF-MIB::ifName']] = None
            continue
        l_device, l_port = interface['_link']
        l_device_name, l_device_dns_domain = l_device.device_fqdn.split('.', 1)
        device_link_info['interfaces'][interface['IF-MIB::ifName']] = {
            'name': l_device_name,
            'dnsDomain': l_device_dns_domain,
            'interface': l_port['IF-MIB::ifName']
        }
    out = {'deviceFqdn': device.device_fqdn, 'topologyStable': args.stable, 'interfaces': device_link_info['interfaces']}

    requests.put('%s/discovery/links' % (args.jaspy_url), json=out)


def send_topology_info(detected_devices):
    for device in detected_devices.values():
        send_device_topology_info(device)



def main():
    rdev = try_resolve(args.root_device)
    if rdev is None:
        logger.error('failed to resolve root device, aborting!')
        return
    detected_devices = {}
    perform_discovery(rdev, detected_devices)
    build_connections(detected_devices)
    send_topology_info(detected_devices)


if __name__ == '__main__':
    main()
