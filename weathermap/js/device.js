import Interface from "./interface.js";
import LinkGroup from "./linkgroup.js";


export default class Device {
    constructor(fqdn) {
        this.fqdn = fqdn;
        this.interfaces = {};
        this.linkGroups = {};
        console.log("create device " + fqdn)
    }

    updateTopologyData(data) {
        console.log("update device " + this.fqdn)
        let linkGroupUpdates = {};
        for(let [key, value] of Object.entries(data["interfaces"])) {
            let iface = null;
            if(!(key in this.interfaces)) {
                this.interfaces[key] = new Interface(key);
            }
            iface = this.interfaces[key];
            iface.updateTopologyData(value);
            if(value["connectedTo"]) {
                let connectionInfo = value["connectedTo"];
                if(!(connectionInfo["fqdn"] in this.linkGroups)) {
                    this.linkGroups[connectionInfo["fqdn"]] = new LinkGroup(connectionInfo["fqdn"]);
                }
                if(!(connectionInfo["fqdn"] in linkGroupUpdates)) {
                    linkGroupUpdates[connectionInfo["fqdn"]] = {};
                }
                linkGroupUpdates[connectionInfo["fqdn"]][value["connectedTo"]["interface"]] = value;
            }
        }
        for(let [key, value] of Object.entries(this.linkGroups)) {
            if(!(key in linkGroupUpdates)) {
                value.destroy();
                delete this.this.linkGroups[key];
                continue;
            }
            value.updateTopologyData(linkGroupUpdates[key]);
        }
    }
}