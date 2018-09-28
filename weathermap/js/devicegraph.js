import Device from "./device.js";


export default class DeviceGraph {
    constructor(viewport) {
        this.viewport = viewport;
        this.linkLayer = new PIXI.DisplayObjectContainer();
        this.deviceLayer = new PIXI.DisplayObjectContainer();
        this.devices = {};
        this.viewport.addChild(this.linkLayer);
        this.viewport.addChild(this.deviceLayer);

        this.updateBatch = {};
    }

    updateTopologyData(data) {
        for(let [key, value] of Object.entries(data["devices"])) {
            let targetDevice = null;
            if(!(key in this.devices)) {
                this.devices[key] = new Device(key);
            }
            targetDevice = this.devices[key]
            targetDevice.updateTopologyData(value);
        }
        for(let [key, value] of Object.entries(this.devices)) {
            if(!(key in data["devices"])) {
                this.devices[key].destroy();
                delete this.devices[key];
            }
        }
        for(let [key, value] of Object.entries(this.devices)) {
            value.updateLinkgroupData(this.devices);
        }
    }

    updateGraphics() {
        let deviceCoordinates = {}
        for(let [key, value] of Object.entries(this.devices)) {
            deviceCoordinates[key] = value.getPosition();
        }
        for(let [key, value] of Object.entries(this.devices)) {
            value.updateGraphics(this.linkLayer, this.deviceLayer, deviceCoordinates);
        }
    }

    beginStatisticsUpdate() {
        this.updateBatch = {};
    }

    commitStatisticsUpdate() {
        for(let [fqdn, data] of Object.entries(this.updateBatch)) {
            if(!(fqdn in this.devices)) {
                console.error("received update for " + fqdn + " which is not in devices");
            } else {
                this.devices[fqdn].updateStatistics(data);
            }
        }
    }

    updateBatchDeviceExists(fqdn) {
        if(!(fqdn in this.updateBatch)) {
            this.updateBatch[fqdn] = {
                "interfaces": [],
                "status": null
            }
        }
    }

    updateBatchDeviceInterfaceExists(fqdn, iface) {
        if(!(fqdn in this.updateBatch)) {
            this.updateBatch[fqdn] = {
                "interfaces": [],
                "status": null
            }
        }
        if(!(iface in this.updateBatch[fqdn]["interfaces"])) {
            this.updateBatch[fqdn]["interfaces"][iface] = {
                "mbps": null,
                "speed_mbps": null,
                "status": null
            }
        }
    }

    updateInterfaceOctetsPerSecond(prometheusResultVector) {
        for(let metric of prometheusResultVector) {
            let direction = metric["metric"]["direction"];
            let fqdn = metric["metric"]["fqdn"];
            let name = metric["metric"]["name"];
            this.updateBatchDeviceInterfaceExists(fqdn, name);
            let timestamp = metric["value"][0];
            let value = parseInt(metric["value"][1]);
            this.updateBatch[fqdn]["interfaces"][name][direction+"_mbps"] = (value * 8.0) / 1000.0 / 1000.0;
        }
    }

    updateInterfaceSpeed(prometheusResultVector) {
        for(let metric of prometheusResultVector) {
            let fqdn = metric["metric"]["fqdn"];
            let name = metric["metric"]["name"];
            this.updateBatchDeviceInterfaceExists(fqdn, name);
            let timestamp = metric["value"][0];
            let value = parseInt(metric["value"][1]);

            this.updateBatch[fqdn]["interfaces"][name]["speed_mbps"] = value;
        }
    }

    updateInterfaceUp(prometheusResultVector) {
        for(let metric of prometheusResultVector) {
            let fqdn = metric["metric"]["fqdn"];
            let name = metric["metric"]["name"];
            this.updateBatchDeviceInterfaceExists(fqdn, name);
            let timestamp = metric["value"][0];
            let value = parseInt(metric["value"][1]);

            this.updateBatch[fqdn]["interfaces"][name]["status"] = value;
        }
    }

    updateDeviceUp(prometheusResultVector) {
        for(let metric of prometheusResultVector) {
            let fqdn = metric["metric"]["fqdn"];
            this.updateBatchDeviceExists(fqdn);
            let timestamp = metric["value"][0];
            let value = parseInt(metric["value"][1]);

            this.updateBatch[fqdn]["status"] = value;
        }
    }
}