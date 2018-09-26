import Device from "./device.js";


export default class DeviceGraph {
    constructor(viewport) {
        this.viewport = viewport;
        this.devices = {}
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
    }
}