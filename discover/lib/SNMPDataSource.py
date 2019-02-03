import requests
import logging
import json
import binascii
from enum import Enum


class DeviceBugs(Enum):
    LLDP_MACADDRESS_DUPLICATE = 1
    LLDP_NO_ASSOCIATION_TO_INTERFACE = 2
    LLDP_MACADDRESS_CANNOT_ASSOCIATE = 3


class SNMPDataSource(object):
    def _build_lldp_mapping_from_mac_address(self, lldp_index, lldp_data, lldp_porttype, lldp_portid):
        if DeviceBugs.LLDP_MACADDRESS_DUPLICATE in self._device_bugs:
            # perhaps lldp-id = ifindex then lol
            if lldp_index in self.interfaces:
                self._lldp_index_to_interface[lldp_index] = self.interfaces[lldp_index]
        elif lldp_portid in self._anything_to_interface:
            self._lldp_index_to_interface[lldp_index] = self._anything_to_interface[lldp_portid]
        else:
            if DeviceBugs.LLDP_MACADDRESS_CANNOT_ASSOCIATE not in self._device_bugs:
                self._device_bugs.append(DeviceBugs.LLDP_MACADDRESS_CANNOT_ASSOCIATE)
                self._logger.error('VENDOR-BUG: cannot associate LLDP ID (MAC) to interface MAC!')
            # perhaps lldp-id = ifindex then lol
            if lldp_index in self.interfaces:
                self._lldp_index_to_interface[lldp_index] = self.interfaces[lldp_index]

    def _lldp_try_map_id_using_anything(self, lldp_index, lldp_data, lldp_porttype, lldp_portid):
        try:
            # try to decode into ascii... :)
            decoded_port = binascii.unhexlify(
                lldp_portid.replace(' ', '')
            ).decode('ascii', errors='ignore')
            if decoded_port.isdigit():
                decoded_port_num = int(decoded_port)
                if decoded_port_num in self.interfaces:
                    self._lldp_index_to_interface[lldp_index] = self.interfaces[decoded_port_num]
                    return
            if decoded_port in self._anything_to_interface:
                self._lldp_index_to_interface[lldp_index] = self._anything_to_interface[decoded_port]
            if decoded_port.startswith('Eth'):
                test_cisco_nexus_quirk = decoded_port.replace('Eth', 'Ethernet')
                if test_cisco_nexus_quirk in self._anything_to_interface:
                    if DeviceBugs.LLDP_NO_ASSOCIATION_TO_INTERFACE not in self._device_bugs:
                        self._device_bugs.append(DeviceBugs.LLDP_NO_ASSOCIATION_TO_INTERFACE)
                        self._logger.error(
                            'VENDOR-BUG: LLDP interface cannot be associated to real interface without guesswork!')
                    self._lldp_index_to_interface[lldp_index] = self._anything_to_interface[test_cisco_nexus_quirk]
        except binascii.Error:
            pass

    def _lldp_handle_lldp_loc_port_table(self, data):
        mac_uniqueness_test = {}
        for entry in data:
            lldp_index = entry['Index']['LLDP-MIB::lldpLocPortNum']
            lldp_data = entry['Objects']
            lldp_porttype = lldp_data['LLDP-MIB::lldpLocPortIdSubtype']
            lldp_portid = lldp_data['LLDP-MIB::lldpLocPortId']
            if lldp_porttype == 'macAddress':
                if DeviceBugs.LLDP_MACADDRESS_DUPLICATE in self._device_bugs:
                    continue
                if lldp_portid in mac_uniqueness_test:
                    self._logger.error('VENDOR-BUG: LLDP macAddress as lldpLocPortId is non-unique!')
                    self._device_bugs.append(DeviceBugs.LLDP_MACADDRESS_DUPLICATE)
                    self._lldp_reliability -= 75
                else:
                    mac_uniqueness_test[lldp_portid] = lldp_index
        for entry in data:
            lldp_index = entry['Index']['LLDP-MIB::lldpLocPortNum']
            lldp_data = entry['Objects']
            lldp_porttype = lldp_data['LLDP-MIB::lldpLocPortIdSubtype']
            lldp_portid = lldp_data['LLDP-MIB::lldpLocPortId']
            if lldp_porttype == 'macAddress':
                self._build_lldp_mapping_from_mac_address(lldp_index, lldp_data, lldp_porttype, lldp_portid)
            else:
                if lldp_portid in self._anything_to_interface:
                    self._lldp_index_to_interface[lldp_index] = self._anything_to_interface[lldp_portid]
                if lldp_porttype in ['local', 'interfaceName']:
                    self._lldp_try_map_id_using_anything(lldp_index, lldp_data, lldp_porttype, lldp_portid)

    def _lldp_handle_lldp_rem_table(self, data):
        for entry in data:
            lldp_index = entry['Index']['LLDP-MIB::lldpRemLocalPortNum']
            lldp_data = entry['Objects']
            if lldp_index in self._lldp_index_to_interface:
                local_interface = self._lldp_index_to_interface[lldp_index]
                if local_interface['IF-MIB::ifName'].endswith('.0'):
                    local_interface = self._anything_to_interface[local_interface['IF-MIB::ifName'][:-2]]
                local_interface['_neighbors']['lldp'] = lldp_data

    def _cdp_handle_cachetable(self, data):
        for cdp_entry in data:
            local_ifindex = cdp_entry['Index']['CISCO-CDP-MIB::cdpCacheIfIndex']
            local_interface = self.interfaces[local_ifindex]
            local_interface['_neighbors']['cdp'] = cdp_entry['Objects']

    def _ifmib_handle_ifindex_tables(self, data):
        for entry in data:
            ifindex = entry['Index']['IF-MIB::ifIndex']
            for key, value in entry['Objects'].items():
                if ifindex not in self.interfaces:
                    self.interfaces[ifindex] = {'_neighbors': {}, 'IF-MIB::ifIndex': ifindex}
                self.interfaces[ifindex][key] = value

    def __init__(self, device_fqdn, snmpbot_address, community):
        self._lldp_reliability = 100
        self._cdp_reliability = 100
        self._device_bugs = []
        self._snmpbot_address = snmpbot_address
        self.device_fqdn = device_fqdn
        self._community = community
        self.interfaces = {}
        self._anything_to_interface = {}
        self._lldp_index_to_interface = {}
        self._kvdata = {}
        self._logger = logging.getLogger('[{}]'.format(device_fqdn))
        self._critical_tables = ['IF-MIB::ifTable', 'IF-MIB::ifXTable']
        self._polling_valid = True
        self._table_handlers = {
            'LLDP-MIB::lldpLocPortTable': self._lldp_handle_lldp_loc_port_table,
            'LLDP-MIB::lldpRemTable': self._lldp_handle_lldp_rem_table,
            'CISCO-CDP-MIB::cdpCacheTable': self._cdp_handle_cachetable,
            'IF-MIB::ifXTable': self._ifmib_handle_ifindex_tables,
            'IF-MIB::ifTable': self._ifmib_handle_ifindex_tables,
        }

    def community(self):
        return self._community

    def valid_result(self):
        return self._polling_valid

    def device_type(self):
        return 'UNKNOWN'

    def os_info(self):
        try:
            return self._kvdata['SNMPv2-MIB::sysDescr']
        except KeyError:
            return 'UNKNOWN'

    def discovery_success(self):
        return 'SNMPv2-MIB::sysDescr' in self._kvdata

    def has_bug(self, bug):
        if bug in self._device_bugs:
            return True
        return False

    def lookup_port_by_cdp_info(self, cdp_cache_device_port):
        if cdp_cache_device_port in self._anything_to_interface:
            return self._anything_to_interface[cdp_cache_device_port]
        return None

    def lookup_port_by_lldp_remote_info(self, lldp_remote_port_id, lldp_remote_port_id_subtype):
        if lldp_remote_port_id_subtype == 'macAddress' and DeviceBugs.LLDP_MACADDRESS_DUPLICATE in self._device_bugs:
            self._logger.error('suffering from LLDP-MACADDRESS-DUPLICATE bug, returning None for lookup by macaddr')
            return None
        if lldp_remote_port_id in self._anything_to_interface:
            return self._anything_to_interface[lldp_remote_port_id]
        if lldp_remote_port_id_subtype in ['local', 'interfaceName']:
            # sometimes this seems to be encoded as hexstr...
            try:
                # try to decode into ascii... :)
                decoded_port = binascii.unhexlify(lldp_remote_port_id.replace(' ', '')).decode('ascii', errors='ignore')
                if decoded_port in self._anything_to_interface:
                    return self._anything_to_interface[decoded_port]
            except binascii.Error:
                pass
        return None

    def lookup_by_unknown_identifier(self, identifier):
        if identifier in self._anything_to_interface:
            return self._anything_to_interface[identifier]
        return None

    def get_chassis_id(self):
        if 'LLDP-MIB::lldpLocChassisId' in self._kvdata:
            return self._kvdata['LLDP-MIB::lldpLocChassisId']
        elif 'BRIDGE-MIB::dot1dBaseBridgeAddress' in self._kvdata:
            return self._kvdata['BRIDGE-MIB::dot1dBaseBridgeAddress']
        else:
            self._logger.error('could not derive chassis id when requested!')
            return None

    def collect(self):
        self._get_bridgemib_values()
        self._get_sysdescr_values()
        self._get_ifmibs()
        self._build_anything_to_interface()
        self._get_lldp_tables()
        self._get_cdp_tables()
        self._ensure_interface_sanity()

    def _ensure_interface_sanity(self):
        for iface in self.interfaces.values():
            if 'IF-MIB::ifName' not in iface:
                iface['IF-MIB::ifName'] = iface['IF-MIB::ifDescr']
            if 'IF-MIB::ifType' not in iface:
                iface['IF-MIB::ifType'] = 'other'

    def _is_valid_mac_keyed_interface(self, interface):
        if 'IF-MIB::ifType' not in interface:
            self._logger.debug('no IF-MIB::ifType for port %s, presume OK', interface['IF-MIB::ifName'])
            return True
        if interface['IF-MIB::ifType'] != 'ethernetCsmacd':
            return False
        return True

    def _build_anything_to_interface(self):
        for ifindex, interface in self.interfaces.items():
            candidate_keys = [
                'IF-MIB::ifAlias',
                'IF-MIB::ifName',
                'IF-MIB::ifDescr'
            ]
            if self._is_valid_mac_keyed_interface(interface):
                candidate_keys += ['IF-MIB::ifPhysAddress']

            for candidate_key in candidate_keys:
                if candidate_key not in interface or interface[candidate_key] is None:
                    continue
                ck_stripped = interface[candidate_key].strip()
                if ck_stripped != '':
                    if candidate_key == 'IF-MIB::ifPhysAddress':
                        ck_spaces = ck_stripped.replace(':', ' ')
                        if ck_stripped in self._anything_to_interface:
                            continue
                        elif ck_spaces in self._anything_to_interface:
                            continue
                        self._anything_to_interface[ck_stripped] = interface
                        self._anything_to_interface[ck_spaces] = interface
                    else:
                        self._anything_to_interface[ck_stripped] = interface

    def _single_object_to_data(self, snmp_object):
        if snmp_object['ID'] == 'LLDP-MIB::lldpLocChassisId':
            snmp_object['Instances'][0]['Value'] = snmp_object['Instances'][0]['Value'].replace(' ', ':')
        self._kvdata[snmp_object['ID']] = snmp_object['Instances'][0]['Value']

    def _get_single_objects(self, single_objects):
        for single_object in single_objects:
            object_resultset = requests.get(
                '{}/api/hosts/{}/objects/{}'.format(self._snmpbot_address, self.device_fqdn, single_object),
                params={
                    'snmp': '{}@{}'.format(self._community, self.device_fqdn)
                }
            )
            try:
                if object_resultset.status_code != 200:
                    continue
                object_resultset_json = object_resultset.json()
                num_results = len(object_resultset_json['Instances'])
                if num_results > 1:
                    self._logger.error('expected <= 1 results for %s, got %s instead!', single_object, num_results)
                    continue
                elif num_results == 1:
                    self._single_object_to_data(object_resultset_json)
            except json.decoder.JSONDecodeError:
                self._logger.error(
                    'error decoding JSON from snmpbot, code=%s data: %s',
                    object_resultset.status_code, object_resultset.text
                )

    def _walk_tables(self, tables):
        for table in tables:
            table_resultset = requests.get(
                '{}/api/hosts/{}/tables/{}'.format(self._snmpbot_address, self.device_fqdn, table),
                params={
                    'snmp': '{}@{}'.format(self._community, self.device_fqdn)
                }
            )
            if table_resultset.status_code != 200:
                if table in self._critical_tables:
                    self._logger.critical('got status=%s for critical table %s', table_resultset.status_code, table)
                    self._polling_valid = False
                else:
                    self._logger.error('got status=%s for table %s', table_resultset.status_code, table)
                continue
            table_resultset_json = table_resultset.json()
            if table_resultset_json['Entries'] is None:
                self._logger.info('empty result for %s', table)
            else:
                self._table_handlers[table](table_resultset_json['Entries'])

    def _get_bridgemib_values(self):
        single_objects = [
            'BRIDGE-MIB::dot1dBaseBridgeAddress'
        ]
        self._get_single_objects(single_objects)

    def _get_sysdescr_values(self):
        single_objects = [
            'SNMPv2-MIB::sysDescr'
        ]
        self._get_single_objects(single_objects)

    def _get_ifmibs(self):
        tables = []
        for candidate in list(self._table_handlers.keys()):
            if candidate.startswith('IF-MIB::'):
                tables.append(candidate)
        self._walk_tables(tables)

    def _get_cdp_tables(self):
        tables = []
        for candidate in list(self._table_handlers.keys()):
            if candidate.startswith('CISCO-CDP-MIB::'):
                tables.append(candidate)
        self._walk_tables(tables)

    def _get_lldp_tables(self):
        single_objects = [
            'LLDP-MIB::lldpLocChassisId'
        ]
        self._get_single_objects(single_objects)
        tables = [
            'LLDP-MIB::lldpLocPortTable',
            'LLDP-MIB::lldpRemTable',
        ]
        self._walk_tables(tables)
