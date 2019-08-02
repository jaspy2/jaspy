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
	"net/http"
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
}

func SNMPBotGetTable(httpClient *http.Client, fqdn string, community string, table string) (*api.Table, error) {
	var url = fmt.Sprintf("%s/api/hosts/%s@%s/tables/%s", snmpBotAPI,  fqdn, community, table)
	resp, err := httpClient.Get(url)
	if err != nil {
		return nil, err
	}

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

func makeFQDN(device *JaspyDevice) string {
	return fmt.Sprintf("%s.%s", device.Name, device.DnsDomain)
}

func GetEntities(httpClient *http.Client, device JaspyDevice) error {

	log.Printf("Started polling device '%s'", device.Name)
	defer log.Printf("Finished polling device '%s'", device.Name)

	var EntityIdentities = make(map[int64]EntityIdentity)
	var fqdn = makeFQDN(&device)

	table, err := SNMPBotGetTable(httpClient, device.SNMPCommunity, fqdn,"ENTITY-MIB::entPhysicalTable")
	if err != nil {
		return err
	}

	for _, entity := range table.Entries {
		entityId := int64(entity.Index["ENTITY-MIB::entPhysicalIndex"].(float64))
		EntityIdentities[entityId] = EntityIdentity{
			Name: entity.Objects["ENTITY-MIB::entPhysicalName"].(string),
			Description: entity.Objects["ENTITY-MIB::entPhysicalDescr"].(string),
			Id: entityId,
		}
	}


	table, err = SNMPBotGetTable(httpClient, device.SNMPCommunity, fqdn, "ENTITY-SENSOR-MIB::entPhySensorTable")
	if err != nil {
		return err
	}

	for _, entity := range table.Entries  {
		entityId := int64(entity.Index["ENTITY-MIB::entPhysicalIndex"].(float64))
		entityIdentity := EntityIdentities[entityId]

		value := entity.Objects["ENTITY-SENSOR-MIB::entPhySensorValue"].(float64)
		scale := entity.Objects["ENTITY-SENSOR-MIB::entPhySensorScale"]
		precision := entity.Objects["ENTITY-SENSOR-MIB::entPhySensorPrecision"].(float64)

		if scale == "milli" {
			value = value / 1000.0
		}

		if precision > 0 {
			value = value / (math.Pow(10, precision))
		}

		if _, ok := metrics[fqdn][entityIdentity.Id]; !ok {
			metrics[fqdn][entityIdentity.Id] = prometheus.NewGauge(prometheus.GaugeOpts{
				Namespace: "jaspy",
				Name: "sensors",
				ConstLabels: map[string]string{
					"hostname": device.Name,
					"fqdn": fqdn,
					"sensor_id": fmt.Sprintf("%d", entityIdentity.Id),
					"sensor_name": entityIdentity.Name,
					"sensor_description": entityIdentity.Description,
					"value_type": entity.Objects["ENTITY-SENSOR-MIB::entPhySensorType"].(string),
				},
			})
			prometheus.MustRegister(metrics[fqdn][entityIdentity.Id])
		}

		metrics[fqdn][entityIdentity.Id].Set(value)


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

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP returned '%s' response whie requesting devices from Jaspy API!",
			resp.Status)
	}

	var jaspyDevices []JaspyDevice
	dec := json.NewDecoder(resp.Body)
	err = dec.Decode(&jaspyDevices)
	if err != nil {
		return nil, fmt.Errorf("Failed to decode JSON from jaspy devices API, %v", err)
	}
	return &jaspyDevices, nil
}

func runOnce() error {

	var wg sync.WaitGroup

	var httpTransport = &http.Transport{
		MaxIdleConns:       10,
		IdleConnTimeout:    30 * time.Second,
		DisableCompression: true,
	}

	var httpClient = http.Client{Transport: httpTransport}


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
		var fqdn = makeFQDN(&device)
		if _, ok := metrics[fqdn]; !ok {
			metrics[fqdn] = make(metricMap)
		}

		go func(client JaspyDevice) {
			defer wg.Done()
			err := GetEntities(&httpClient, client)
			if err != nil {
				log.Printf("Error %+v", err)
			}
		}(device)
		wg.Add(1)
	}
	wg.Wait()
	return nil
}

func main() {

	metrics = make(deviceMetricMap)

	flag.StringVar(&snmpBotAPI, "snmbot-api", "http://localhost:8286", "Base path to SNMPBot API")
	flag.StringVar(&jaspyAPI, "jaspy-api", "http://0.0.0.0:8000", "Base path to Jaspy API")
	flag.DurationVar(&PollInterval, "poll-interval", 120*time.Second, "Polling interval")
	flag.StringVar(&listenAddress, "listen-address", "localhost:8098", "metrics listen address")

	flag.Parse()

	http.Handle("/metrics", promhttp.Handler())
	go http.ListenAndServe(listenAddress, nil)

	var timer chan bool = make(chan bool, 1024)

	go func(timer chan bool) {
		for {
			<- time.After(PollInterval)
			timer <- true
		}
	}(timer)

	for {
		runOnce()
		<-timer
	}
}