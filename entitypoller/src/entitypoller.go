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

var (
	snmpBotAPI string
	jaspyAPI string
	listenAddress string
	PollInterval time.Duration
	metrics deviceMetricMap
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

		if _, ok := metrics[device.FQDN][entityIdentity.Id]; !ok {
			metrics[device.FQDN][entityIdentity.Id] = prometheus.NewGauge(prometheus.GaugeOpts{
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
			prometheus.MustRegister(metrics[device.FQDN][entityIdentity.Id])
		}

		metrics[device.FQDN][entityIdentity.Id].Set(value)
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

		if _, ok := metrics[device.FQDN]; !ok {
			metrics[device.FQDN] = make(metricMap)
		}

		go func(client JaspyDevice) {
			defer wg.Done()
			sleep := (rand.Int63() % int64(interval/time.Millisecond)) / 2
			<-time.After(time.Duration(sleep) * time.Millisecond)
			err := GetEntities(&httpClient, client)
			if err != nil {
				log.Printf("Error %+v while fetching device %s", err, client.FQDN)
			}
		}(device)
		wg.Add(1)
	}
	wg.Wait()

	return nil
}

func main() {

	metrics = make(deviceMetricMap)

	flag.StringVar(&snmpBotAPI, "snmpbot-api", "http://localhost:8286", "Base path to SNMPBot API")
	flag.StringVar(&jaspyAPI, "jaspy-api", "http://0.0.0.0:8000", "Base path to Jaspy API")
	flag.DurationVar(&PollInterval, "poll-interval", 120*time.Second, "Polling interval")
	flag.StringVar(&listenAddress, "listen-address", "localhost:8098", "metrics listen address")

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