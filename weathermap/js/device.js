import Interface from "./interface.js";
import LinkGroup from "./linkgroup.js";


export default class Device {
    constructor(fqdn) {
        this.fqdn = fqdn;
        this.interfaces = {};
        this.linkGroups = {};
        this.graphicsObjectInfo = null;
        this.position = new Victor(Math.random()*1600,Math.random()*400);
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
                    this.linkGroups[connectionInfo["fqdn"]] = new LinkGroup(connectionInfo["fqdn"]);
                }
                if(!(connectionInfo["fqdn"] in linkGroupUpdates)) {
                    linkGroupUpdates[connectionInfo["fqdn"]] = {};
                }
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

    updateGraphics(viewport) {
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Sprite(PIXI.Texture.WHITE),
                "attachedTo": viewport
            };
            this.graphicsObjectInfo["object"].tint = 0xff0000;
            this.graphicsObjectInfo["object"].width = this.graphicsObjectInfo["object"].height = 32
            this.graphicsObjectInfo["object"].position.set(this.position.x, this.position.y);
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }
    }

    getPosition() {
        return this.position;
    }
}