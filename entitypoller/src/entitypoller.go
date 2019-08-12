package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"github.com/qmsk/snmpbot/api"
	"log"
	"math"
	"math/rand"
	"net/http"
	"strings"
	"sync"
	"time"
)

type metricMap map[int64]prometheus.Gauge
type deviceMetricMap map[string]metricMap
type stringMetricMap map[string]metricMap

var (
	snmpBotAPI    string
	jaspyAPI      string
	listenAddress string
	disableSensors bool = false
	disableSTP bool = false
	PollInterval  time.Duration
	entityMetrics deviceMetricMap
	stpMetrics map[string]stringMetricMap
)

const (
 RSTPPORTROLE string = "RSTPPORTROLE"
 IFINDEX      string = "ifIndex"
 IFDESCR      string = "ifDescr"
)

type EntityIdentity struct {
	Name string
	Id int64
	Description string
}


type JaspyInterface struct {
	Id int64 `json:"id"`
	Index int64 `json:"index"`
	InterfaceType string `json:"interfaceType"`
	DeviceId int64 `json:"deviceId"`
	DisplayName string `json:"displayName"`
	Name string `json:"name"`
	Alias string `json:"alias"`
	Description string `json:"description"`
	PollingEnabled *bool `json:"pollingEnabled"`
	SpeedOverride *int64 `json:"speedOverride"`
	VirtualConnection *int64 `json:"virtualConnection"`
}

type JaspyDevice struct {
	Id int64 `json:"id"`
	Name string `json:"name"`
	DnsDomain string `json:"dnsDomain"`
	SNMPCommunity string `json:"snmpCommunity"`
	BaseMAC string `json:"baseMac"`
	PollingEnabled *bool `json:"pollingEnabled"`
	OsInfo string `json:"osInfo"`
	DeviceType string `json:"deviceType"`
	SoftwareVersion string `json:"softwareVersion"`
	FQDN string `json:"omitempty"`
	Interfaces map[string]*JaspyInterface `json:"omitempty"`
}

type stringNumeric interface {
	GetNumeric() float64
	GetHelp() string
}

type RSTPPortRole string

func (s RSTPPortRole) GetNumeric() float64 {
	var value float64 = 0

	switch string(s) {
	case "disabled":
		value = 1
	case "root":
		value = 2
	case "designated":
		value = 3
	case "alternate":
		value = 4
	case "backUp":
		value = 5
	case "boundary":
		value = 6
	case "master":
		value = 7
	}
	return value
}

func (s RSTPPortRole) GetHelp() string {
	return "Enumeration (1-disabled, 2-root, 3-designated, 4-alternate, 5-backUp, 6-boundary, 7-master)"
}

type STPPortState string

func (s STPPortState) GetNumeric() float64 {
	var value float64 = 0
	switch string(s) {
	case "disabled":
		value = 1
	case "blocking":
		value = 2
	case "listening":
		value = 3
	case "learning":
		value = 4
	case "forwarding":
		value = 5
	case "broken":
		value = 6
	}
	return value
}

func (s STPPortState) GetHelp() string {
	return "Enumeration (1-disabled, 2-blocking, 3-listening, 4-learning, 5-forwarding, 6-broken)"
}

type STPPortEnable string

func (s STPPortEnable) GetNumeric() float64 {
	var value float64 = 0
	switch string(s) {
	case "enabled":
		value = 1
	case "disabled":
		value = 2
	}
	return value
}

func (s STPPortEnable) GetHelp() string {
	return "Enumeration (1-enabled, 2-disabled)"
}

func SNMPBotGetTable(httpClient *http.Client, fqdn string, community string, table string) (*api.Table, error) {
	var url = fmt.Sprintf("%s/api/hosts/%s@%s/tables/%s", snmpBotAPI,  fqdn, community, table)
	resp, err := httpClient.Get(url)

	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP returned '%s' response while requesting %s!", resp.Status, url)
	}

	var tableIndex api.Table
	dec := json.NewDecoder(resp.Body)
	err = dec.Decode(&tableIndex)
	if err != nil {
		return nil, fmt.Errorf("Failed to decode JSON from SNMPBOT API, %v", err)
	}
	return &tableIndex, nil
}

func GetEntitiesByPhysicalIndex(httpClient *http.Client,  device *JaspyDevice,
	entityIdentities *map[int64]EntityIdentity, valueField string, scaleField string, precisionField string,
	valueTypeField string, tableField string) error {
	table, err := SNMPBotGetTable(httpClient, device.SNMPCommunity, device.FQDN,
		tableField)
	if err != nil {
		return err
	}
	for _, entity := range table.Entries  {
		entityId := int64(entity.Index["ENTITY-MIB::entPhysicalIndex"].(float64))
		entityIdentity := (*entityIdentities)[entityId]

		if entity.Objects[valueField] == nil {
			log.Printf("Skipping entity %d with nil value", entityId)
			continue
		}
		value := entity.Objects[valueField].(float64)

		if entity.Objects[valueTypeField] == nil {
			log.Printf("Skipping entity %d with nil valuetype", entityId)
			continue
		}
		valueType := entity.Objects[valueTypeField].(string)

		if entity.Objects[scaleField] == nil {
			log.Printf("Skipping entity %d with nil scale", entityId)
			continue
		}
		scale := entity.Objects[scaleField].(string)

		if entity.Objects[precisionField] == nil {
			log.Printf("Skipping entity %d with nil precision", entityId)
			continue
		}
		precision := entity.Objects[precisionField].(float64)

		if scale == "milli" {
			value = value / 1000.0
		}

		if precision > 0 {
			value = value / (math.Pow(10, precision))
		}

		// Check if name starts with interface name
		nameToCheck := strings.Split(entityIdentity.Name, " ")[0]

		var interfaceName string
		var interfaceId int64
		iface, ok := device.Interfaces[nameToCheck]
		if ok {
			interfaceName = iface.Name
			interfaceId = iface.Id
		}

		if _, ok := entityMetrics[device.FQDN][entityIdentity.Id]; !ok {
			entityMetrics[device.FQDN][entityIdentity.Id] = prometheus.NewGauge(prometheus.GaugeOpts{
				Namespace: "jaspy",
				Name: "sensors",
				ConstLabels: map[string]string{
					"hostname": device.Name,
					"fqdn": device.FQDN,
					"sensor_id": fmt.Sprintf("%d", entityIdentity.Id),
					"sensor_name": entityIdentity.Name,
					"sensor_description": entityIdentity.Description,
					"value_type": valueType,
					"interface_name": interfaceName,
					"interface_id": fmt.Sprintf("%d", interfaceId),
				},
			})
			prometheus.MustRegister(entityMetrics[device.FQDN][entityIdentity.Id])
		}

		entityMetrics[device.FQDN][entityIdentity.Id].Set(value)
	}
	return nil
}

func GetCiscoEntities(httpClient *http.Client,  device *JaspyDevice, entityIdentities *map[int64]EntityIdentity) error {
	return GetEntitiesByPhysicalIndex(
		httpClient,
		device,
		entityIdentities,
		"CISCO-ENTITY-SENSOR-MIB::entSensorValue",
		"CISCO-ENTITY-SENSOR-MIB::entSensorScale",
		"CISCO-ENTITY-SENSOR-MIB::entSensorPrecision",
		"CISCO-ENTITY-SENSOR-MIB::entSensorType",
		"CISCO-ENTITY-SENSOR-MIB::entSensorValueTable")
}

func GetStandardEntities(httpClient *http.Client,  device *JaspyDevice, entityIdentities *map[int64]EntityIdentity) error {
	return GetEntitiesByPhysicalIndex(
		httpClient,
		device,
		entityIdentities,
		"ENTITY-SENSOR-MIB::entPhySensorValue",
		"ENTITY-SENSOR-MIB::entPhySensorScale",
		"ENTITY-SENSOR-MIB::entPhySensorPrecision",
		"ENTITY-SENSOR-MIB::entPhySensorType",
		"ENTITY-SENSOR-MIB::entPhySensorTable")
}

func GetEntities(httpClient *http.Client, device JaspyDevice) error {
	tstart := time.Now()
	log.Printf("Started polling device '%s'", device.Name)
	defer func () {
		log.Printf("Finished polling device '%s' in %.2f s", device.Name, float64(time.Now().Sub(tstart)/1000/1000)/1000.0)
	}()

	var entityIdentities = make(map[int64]EntityIdentity)

	table, err := SNMPBotGetTable(httpClient, device.SNMPCommunity, device.FQDN,"ENTITY-MIB::entPhysicalTable")
	if err != nil {
		return err
	}

	for _, entity := range table.Entries {
		entityId := int64(entity.Index["ENTITY-MIB::entPhysicalIndex"].(float64))
		entityIdentities[entityId] = EntityIdentity{
			Name: entity.Objects["ENTITY-MIB::entPhysicalName"].(string),
			Description: entity.Objects["ENTITY-MIB::entPhysicalDescr"].(string),
			Id: entityId,
		}
	}


	err = GetStandardEntities(httpClient, &device, &entityIdentities)
	if err != nil {
		log.Printf("Failed to get standard entities")
	}

	err = GetCiscoEntities(httpClient, &device, &entityIdentities)
	if err != nil {
		log.Printf("Failed to get cisco entities")
	}

	return nil
}

func setVlanStpMetric(device *JaspyDevice, details *map[string]string, vlan int64, ifidx int64, key string,
	value float64, help string) error {
	keyName := fmt.Sprintf("%s@%d", key, vlan)
	if _, ok := stpMetrics[device.FQDN][keyName]; !ok {
		stpMetrics[device.FQDN][keyName] = make(metricMap)
	}

	if _, ok := stpMetrics[device.FQDN][keyName][ifidx]; !ok {
		stpMetrics[device.FQDN][keyName][ifidx] = prometheus.NewGauge(prometheus.GaugeOpts{
			Namespace: "jaspy",
			Subsystem: "stp",
			Name: key,
			Help: help,
			ConstLabels: map[string]string{
				"hostname": device.Name,
				"fqdn": device.FQDN,
				"vlan": fmt.Sprintf("%d", vlan),
				"stp_port_id": fmt.Sprintf("%d", ifidx),
				"interface_name": (*details)[IFDESCR],
				"interface_id": (*details)[IFINDEX],
			},
		})
		prometheus.MustRegister(stpMetrics[device.FQDN][keyName][ifidx])
	}
	stpMetrics[device.FQDN][keyName][ifidx].Set(value)
	return nil
}

func GetIfTable(httpClient *http.Client, device *JaspyDevice) (*map[int64]string, error) {
	var interfaces = make(map[int64]string)

	table, err := SNMPBotGetTable(httpClient, device.SNMPCommunity, device.FQDN,"IF-MIB::ifTable")
	if err != nil {
		return nil, err
	}

	for _, entry  := range table.Entries {
		ifidx := int64(entry.Index["IF-MIB::ifIndex"].(float64))
		interfaces[ifidx] = entry.Objects["IF-MIB::ifDescr"].(string)
	}
	return &interfaces, nil
}

func GetSTP(httpClient *http.Client, device JaspyDevice) error {
	table, err := SNMPBotGetTable(httpClient, device.SNMPCommunity, device.FQDN,
		"CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleTable")
	if err != nil {
		return err
	}

	var vlans map[int64]map[int64]map[string]string
	vlans = make(map[int64]map[int64]map[string]string)

	for _, entry := range table.Entries {
		var vlan, ifidx int64
		var status string
		if vlanVal, ok := entry.Index["CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex"]; ok {
			vlan = int64(vlanVal.(float64))
		} else {
			log.Printf("%s: No CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleInstanceIndex in response", device.FQDN)
			continue
		}
		if ifidxVal, ok := entry.Index["CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRolePortIndex"]; ok {
			ifidx = int64(ifidxVal.(float64))
		} else {
			log.Printf("%s: No CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRolePortIndex in response", device.FQDN)
			continue
		}
		if statusVal, ok := entry.Objects["CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleValue"]; ok {
			status = statusVal.(string)
		} else {
			log.Printf("%s: No CISCO-STP-EXTENSIONS-MIB::stpxRSTPPortRoleValue in response", device.FQDN)
			continue
		}
		if _, ok := vlans[vlan]; !ok {
			vlans[vlan] = make(map[int64]map[string]string)
		}
		if _, ok := vlans[vlan][ifidx]; !ok {
			vlans[vlan][ifidx] = make(map[string]string)
		}
		vlans[vlan][ifidx][RSTPPORTROLE] = status
		vlans[vlan][ifidx][IFINDEX] = "0"
		vlans[vlan][ifidx][IFDESCR] = "UNKNOWN"
	}

	// Get ifTable for names

	interfaces, err := GetIfTable(httpClient, &device)
	if err != nil {
		return err
	}

	for vlan := range vlans {
		// Get ifIndex from dot1dBasePortTable
		perVlanCommunity := fmt.Sprintf("%s@%d", device.SNMPCommunity, vlan)
		table, err := SNMPBotGetTable(httpClient, perVlanCommunity, device.FQDN,
			"BRIDGE-MIB::dot1dBasePortTable")
		if err != nil {
			return err
		}
		for _, entry  := range table.Entries {
			ifidx := int64(entry.Index["BRIDGE-MIB::dot1dBasePort"].(float64))

			var realifidx int64
			if realIfIdxVal, ok := entry.Objects["BRIDGE-MIB::dot1dBasePortIfIndex"]; ok {
				realifidx = int64(realIfIdxVal.(float64))
			} else {
				log.Printf("%s vlan %d: No BRIDGE-MIB::dot1dBasePortIfIndex in response", vlan, device.FQDN)
				continue
			}

			if _, ok := vlans[vlan][ifidx]; !ok {
				continue
			}

			vlans[vlan][ifidx][IFINDEX] = fmt.Sprintf("%d", realifidx)
			if ifdecr, ok := (*interfaces)[realifidx]; ok {
				vlans[vlan][ifidx][IFDESCR] = ifdecr
			}
		}

		// Get Cost, state etc. from

		table, err = SNMPBotGetTable(httpClient, perVlanCommunity, device.FQDN,
			"BRIDGE-MIB::dot1dStpPortTable")
		if err != nil {
			return err
		}
		for _, entry  := range table.Entries {
			ifidx := int64(entry.Index["BRIDGE-MIB::dot1dStpPort"].(float64))
			if _, ok := vlans[vlan][ifidx]; !ok {
				continue
			}

			stpPortDesignatedCost := entry.Objects["BRIDGE-MIB::dot1dStpPortDesignatedCost"].(float64)
			stpPortPathCost := entry.Objects["BRIDGE-MIB::dot1dStpPortPathCost"].(float64)
			stpPortPriority := entry.Objects["BRIDGE-MIB::dot1dStpPortPriority"].(float64)
			stpPortForwardTransitions := entry.Objects["BRIDGE-MIB::dot1dStpPortForwardTransitions"].(float64)
			//stpPortDesignatedRoot := entry.Objects["BRIDGE-MIB::dot1dStpPortDesignatedRoot"].(string)
			//stpPortDesignatedBridge := entry.Objects["BRIDGE-MIB::dot1dStpPortDesignatedBridge"].(string)
			stpPortEnable := STPPortEnable(entry.Objects["BRIDGE-MIB::dot1dStpPortEnable"].(string))
			stpPortState := STPPortState(entry.Objects["BRIDGE-MIB::dot1dStpPortState"].(string))
			stpPortRole := RSTPPortRole(vlans[vlan][ifidx][RSTPPORTROLE])

			thisIf := vlans[vlan][ifidx]

			setVlanStpMetric(&device, &thisIf, vlan, ifidx,"port_designated_cost", stpPortDesignatedCost,
				"BRIDGE-MIB::dot1dStpPortDesignatedCost")
			setVlanStpMetric(&device, &thisIf, vlan, ifidx,"port_path_cost", stpPortPathCost,
				"BRIDGE-MIB::dot1dStpPortPathCost")
			setVlanStpMetric(&device, &thisIf, vlan, ifidx,"port_priority", stpPortPriority,
				"BRIDGE-MIB::dot1dStpPortPriority")
			setVlanStpMetric(&device, &thisIf, vlan, ifidx,"port_forward_transitions", stpPortForwardTransitions,
				"BRIDGE-MIB::dot1dStpPortForwardTransitions")
			setVlanStpMetric(&device, &thisIf, vlan, ifidx,"port_enabled", stpPortEnable.GetNumeric(),
				stpPortEnable.GetHelp())
			setVlanStpMetric(&device, &thisIf, vlan, ifidx,"port_role", stpPortRole.GetNumeric(),
				stpPortRole.GetHelp())
			setVlanStpMetric(&device, &thisIf, vlan, ifidx,"port_state", stpPortState.GetNumeric(),
				stpPortState.GetHelp())
		}
	}

	return nil
}

func getJaspyDevices(httpClient *http.Client) (*[]JaspyDevice, error) {
	log.Printf("Started getting jaspy devices")
	defer log.Printf("Finished getting jaspy devices")
	resp, err := httpClient.Get(fmt.Sprintf("%s/dev/device/", jaspyAPI))

	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP returned '%s' response whie requesting devices from Jaspy API!",
			resp.Status)
	}

	var jaspyDevices []JaspyDevice
	dec := json.NewDecoder(resp.Body)
	err = dec.Decode(&jaspyDevices)
	if err != nil {
		return nil, fmt.Errorf("failed to decode JSON from jaspy devices API, %v", err)
	}
	return &jaspyDevices, nil
}

func getJaspyDeviceInterfaces(httpClient *http.Client, device *JaspyDevice) error {
	log.Printf("Started getting jaspy interfaces for %s", device.FQDN)
	defer log.Printf("Finished getting jaspy interfaces %s", device.FQDN)
	resp, err := httpClient.Get(fmt.Sprintf("%s/dev/device/%s/interfaces", jaspyAPI, device.FQDN))

	if err != nil {
		return err
	}
	defer resp.Body.Close()

	var jaspyInterfaces []JaspyInterface
	dec := json.NewDecoder(resp.Body)
	err = dec.Decode(&jaspyInterfaces)
	if err != nil {
		return fmt.Errorf("failed to decode JSON from jaspy device interfaces API, %v", err)
	}

	device.Interfaces = make(map[string]*JaspyInterface)

	for idx, iface := range jaspyInterfaces {
		device.Interfaces[iface.Name] = &jaspyInterfaces[idx]
		device.Interfaces[iface.Description] = &jaspyInterfaces[idx]
	}
	return nil
}


func runOnce(interval time.Duration, httpTransport *http.Transport) error {

	var wg sync.WaitGroup

	var httpClient = http.Client{Transport: httpTransport, Timeout: time.Second * 30}

	clients, err := getJaspyDevices(&httpClient)

	if err != nil {
		log.Printf("Failed to get jaspy devices: %s", err)
		return err
	}

	for _, device := range *clients {
		if device.PollingEnabled != nil {
			polling := *device.PollingEnabled
			if polling == false {
				continue
			}
		}
		device.FQDN = fmt.Sprintf("%s.%s", device.Name, device.DnsDomain)

		err = getJaspyDeviceInterfaces(&httpClient, &device)

		if err != nil {
			log.Printf("Failed to fetch jaspy interfaces for device %s", device.FQDN)
			continue
		}

		if _, ok := entityMetrics[device.FQDN]; !ok {
			entityMetrics[device.FQDN] = make(metricMap)
		}

		if _, ok := stpMetrics[device.FQDN]; !ok {
			stpMetrics[device.FQDN] = make(stringMetricMap)
		}

		go func(client JaspyDevice) {
			defer wg.Done()
			sleep := (rand.Int63() % int64(interval/time.Millisecond)) / 2
			<-time.After(time.Duration(sleep) * time.Millisecond)
			if !disableSensors {
				err := GetEntities(&httpClient, client)
				if err != nil {
					log.Printf("Error %+v while fetching device %s", err, client.FQDN)
				}
			}
			if !disableSTP {
				err = GetSTP(&httpClient, client)
				if err != nil {
					log.Printf("Error %+v while fetching device %s", err, client.FQDN)
				}
			}
		}(device)
		wg.Add(1)
	}
	wg.Wait()

	return nil
}

func main() {

	entityMetrics = make(deviceMetricMap)
	stpMetrics = make(map[string]stringMetricMap)

	flag.StringVar(&snmpBotAPI, "snmpbot-api", "http://localhost:8286", "Base path to SNMPBot API")
	flag.StringVar(&jaspyAPI, "jaspy-api", "http://0.0.0.0:8000", "Base path to Jaspy API")
	flag.DurationVar(&PollInterval, "poll-interval", 120*time.Second, "Polling interval")
	flag.StringVar(&listenAddress, "listen-address", "localhost:8098", "metrics listen address")
	flag.BoolVar(&disableSensors, "disable-sensors", false, "Disable sensors")
	flag.BoolVar(&disableSTP, "disable-stp", false, "Disable STP")

	flag.Parse()

	http.Handle("/metrics", promhttp.Handler())
	go http.ListenAndServe(listenAddress, nil)

	var httpTransport = &http.Transport{
		MaxIdleConns:       10,
		IdleConnTimeout:    30 * time.Second,
		DisableCompression: true,
		TLSHandshakeTimeout: 5 * time.Second,
	}

	var timer chan bool = make(chan bool, 1024)

	go func(timer chan bool) {
		for {
			<- time.After(PollInterval)
			timer <- true
		}
	}(timer)

	for {
		runOnce(PollInterval, httpTransport)
		<-timer
	}

}