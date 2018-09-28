import Interface from "./interface.js";
import LinkGroup from "./linkgroup.js";


export default class Device {
    constructor(fqdn) {
        this.fqdn = fqdn;
        this.interfaces = {};
        this.linkGroups = {};
        this.graphicsObjectInfo = null;
        this.dirty = false;
        this.status = null;
        this.setPosition(new Victor(Math.random()*1600,Math.random()*400));
        console.log("create device " + fqdn)
    }

    destroy() {
        console.log("destroy device " + this.fqdn)
        for(let [key, value] of Object.entries(this.interfaces)) {
            value.destroy();
            delete this.interfaces[key];
        }
        for(let [key, value] of Object.entries(this.linkGroups)) {
            value.destroy();
            delete this.linkGroups[key];
        }

        if(this.graphicsObjectInfo !== null) {
            this.graphicsObjectInfo["attachedTo"].removeChild(this.graphicsObjectInfo["object"]);
            this.graphicsObjectInfo = null;
        }
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
                    this.linkGroups[connectionInfo["fqdn"]] = new LinkGroup(this.fqdn, connectionInfo["fqdn"]);
                }
                if(!(connectionInfo["fqdn"] in linkGroupUpdates)) {
                    linkGroupUpdates[connectionInfo["fqdn"]] = {};
                }
                value["interface"] = iface;
                linkGroupUpdates[connectionInfo["fqdn"]][value["connectedTo"]["interface"]] = value;
            }
        }
        for(let [key, value] of Object.entries(this.interfaces)) {
            if(!(key in data["interfaces"])) {
                value.destroy();
                delete this.interfaces[key];
            }
        }
        for(let [key, value] of Object.entries(this.linkGroups)) {
            if(!(key in linkGroupUpdates)) {
                value.destroy();
                delete this.linkGroups[key];
                continue;
            }
            value.updateTopologyData(linkGroupUpdates[key]);
        }
    }

    updateLinkgroupData(devices) {
        for(let [key, value] of Object.entries(this.linkGroups)) {
            if(!(key in devices)) {
                console.error("device " + key + " referenced by linkgroup not in devices!?");
                continue;
            }
            value.updateTargetInfo(devices[key]);
        }
    }

    updateGraphics(linkLayer, deviceLayer, deviceCoordinates) {
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": deviceLayer
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }

        if(this.dirty) {
            let obj = this.graphicsObjectInfo["object"];
            obj.clear();
            let fillcolor = null;
            if(this.status === 1) {
                fillcolor = 0x00aa00;
            } else if(this.status === 0) {
                fillcolor = 0xff0000;
            } else {
                fillcolor = 0xffff00;
            }
            obj.beginFill(fillcolor);
            obj.lineStyle(2,0x777777);
            obj.moveTo(-10,-10);
            obj.lineTo(-10,10); obj.lineTo(10,10); obj.lineTo(10,-10); obj.lineTo(-10,-10);
            obj.endFill();
            obj.position.set(this.position.x, this.position.y);
            this.dirty = false;
        }

        for(let [key, value] of Object.entries(this.linkGroups)) {
            let localCoord = this.getPosition();
            let remoteCoord = deviceCoordinates[key];
            value.updateGraphics(linkLayer, localCoord, remoteCoord);
        }
    }

    getPosition() {
        return this.position;
    }

    setPosition(position) {
        this.position = position;
        this.dirty = true;
    }

    updateStatistics(data) {
        this.status = data["status"];
        for(let [key, value] of Object.entries(data["interfaces"])) {
            if(!(key in this.interfaces)) {
                console.error("received update for " + fqdn + "/" + key + " which is not in device interfaces");
            } else {
                this.interfaces[key].updateStatistics(value);
            }
        }
    }
}