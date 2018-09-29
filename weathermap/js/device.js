import Interface from "./interface.js";
import LinkGroup from "./linkgroup.js";


export default class Device {
    constructor(fqdn) {
        this.fqdn = fqdn;
        this.hostname = fqdn.split('.', 2)[0];
        this.interfaces = {};
        this.linkGroups = {};
        this.graphicsObjectInfo = null;
        this.graphicsDirty = false;
        this.status = null;
        this.dragInfo = null;
        this.setPosition(new Victor(Math.random()*1600,Math.random()*400));
        console.log("create device " + fqdn)
    }

    armDragEvents(object) {
        object.device = this;
        object.interactive = true;
        object.buttonMode = true;

        object.mousedown = object.touchstart = function(data) {
            this.device.dragInfo = {};
            simulationGlobals.requestGraphicsUpdate = true;
            simulationGlobals.requestAnimationUpdate = true;
            data.stopPropagation();
        };

        object.mouseup = object.mouseupoutside = object.touchend = object.touchendoutside = function(data) {
            this.device.dragInfo = null;
        };

        object.mousemove = object.touchmove = function(data) {
            if(this.device.dragInfo !== null) {
                let curPoint = data.data.global;
                let worldPoint = simulationGlobals.viewport.toWorld(curPoint);
                this.device.setPosition(new Victor(worldPoint.x, worldPoint.y));
                simulationGlobals.requestGraphicsUpdate = true;
                simulationGlobals.requestAnimationUpdate = true;
                data.stopPropagation();
            }
        };
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
            this.armDragEvents(this.graphicsObjectInfo["object"]);

            let text = new PIXI.Text(this.hostname, {fontFamily : '"Courier New", Courier, monospace', fontSize: 14, fill : 0xffffff, align : 'center'});
            text.position.x -= text.width / 2.0;
            text.position.y -= 30;
            this.graphicsObjectInfo["object"].addChild(text);
        }

        if(this.graphicsDirty) {
            let obj = this.graphicsObjectInfo["object"];
            obj.clear();
            let fillcolor = null;
            if(this.status === true) {
                fillcolor = 0x00aa00;
            } else if(this.status === false) {
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
            this.graphicsDirty = false;
        }

        for(let [key, value] of Object.entries(this.linkGroups)) {
            let localCoord = this.getPosition();
            let remoteCoord = deviceCoordinates[key];
            value.updateGraphics(linkLayer, localCoord, remoteCoord);
        }
    }

    setStatus(newStatus) {
        this.status = newStatus["state"];
        this.graphicsDirty = true;

        for(let [key, value] of Object.entries(newStatus["interfaces"])) {
            if(!(key in this.interfaces)) {
                console.error("received update for " + fqdn + "/" + key + " which is not in device interfaces");
            } else {
                this.interfaces[key].setStatus(value);
            }
        }

        for(let [key, value] of Object.entries(this.linkGroups)) {
            value.interfacesUpdated();
        }

        simulationGlobals.requestGraphicsUpdate = true;
    }

    getPosition() {
        return this.position;
    }

    setPosition(position) {
        position = new Victor(Math.round(position.x), Math.round(position.y));
        if(this.position && (position.x == this.position.x && position.y == this.position.y)) {
            return;
        }
        this.position = position;
        this.graphicsDirty = true;
    }

    updateStatistics(data) {
        for(let [key, value] of Object.entries(data["interfaces"])) {
            if(!(key in this.interfaces)) {
                console.error("received update for " + fqdn + "/" + key + " which is not in device interfaces");
            } else {
                this.interfaces[key].updateStatistics(value);
            }
        }

        for(let [key, value] of Object.entries(this.linkGroups)) {
            value.interfacesUpdated();
        }
        simulationGlobals.requestGraphicsUpdate = true;
    }
}