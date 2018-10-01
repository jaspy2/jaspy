import Interface from "./interface.js";
import LinkGroup from "./linkgroup.js";
import FocusEffect from "./effects/focuseffect.js";
import {hostnameFromFQDN} from "./util.js";


export default class Device {
    constructor(fqdn) {
        this.fqdn = fqdn;
        this.hostname = hostnameFromFQDN(fqdn);
        this.interfaces = {};
        this.linkGroups = {};
        this.graphicsObjectInfo = null;
        this.graphicsDirty = false;
        this.status = null;
        this.dragInfo = null;
        this.attachedEffects = [];
        this.position = new Victor(0,0);
        this.setPosition(new Victor(Math.random()*1600,Math.random()*400));
        this.requestedPosition = null;

        this.neighborDevices = [];
        this.superNode = false;
        this.expanded = false;
        
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
            simulationGlobals.positionUpdatesFrozen = true;
            data.stopPropagation();
        };

        object.mouseup = object.mouseupoutside = object.touchend = object.touchendoutside = function(data) {
            this.device.dragInfo = null;
            let fetchInfo = {
                method: 'PUT',
                body: JSON.stringify({
                    deviceFqdn: this.device.fqdn,
                    x: this.device.position.x,
                    y: this.device.position.y,
                    superNode: false,
                    expandedByDefault: false
                }),
                headers: {
                    'Content-Type': 'application/json',
                },
            }
            fetch(config.jaspyNexusURL+"/position", fetchInfo).then(function() {
                simulationGlobals.positionUpdatesFrozen = false;
            }).catch(function() {
                simulationGlobals.positionUpdatesFrozen = false;
            });
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
                this.interfaces[key] = new Interface(key, this.fqdn);
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
        let newNeighborDeviceList = [];
        for(let [key, value] of Object.entries(this.linkGroups)) {
            if(!(key in devices)) {
                console.error("device " + key + " referenced by linkgroup not in devices!?");
                continue;
            }
            newNeighborDeviceList.push(devices[key]);
            value.updateTargetInfo(devices[key]);
        }
        this.neighborDevices = newNeighborDeviceList;
        if(this.neighborDevices.length > 1) {
            this.superNodeByLinks = true;
        } else {
            this.superNodeByLinks = false;
        }
    }

    superNodeAnimation() {
        if(this.requestedPosition !== null) {
            let offset = this.requestedPosition.clone().subtract(this.position);
            if(offset.length() < 0.25) {
                this.setPosition(this.requestedPosition);
                this.requestedPosition = null;
            } else {
                let factoredOffset = offset.clone().multiply(new Victor(0.1,0.1));
                this.setPosition(this.position.clone().add(factoredOffset), true);
            }
            simulationGlobals.requestGraphicsUpdate = true;
            simulationGlobals.requestAnimationUpdate = true;
        }
    }

    edgeNodeAnimation() {
        let offsetVector = new Victor(0,0);
        if(this.neighborDevices.length === 1) {
            let myPosition = this.getPosition().clone();
            let nbrPosition = this.neighborDevices[0].getPosition();

            let dirvToNbr = nbrPosition.clone().subtract(myPosition).normalize();
            let springTarget = nbrPosition.clone().subtract(dirvToNbr.clone().multiply(new Victor(256,256)));
            let dirvToSpringTarget = springTarget.subtract(myPosition);
            let smoothedOffset = dirvToSpringTarget.clone().multiply(new Victor(0.2,0.2));

            if(dirvToSpringTarget.length() < 1) {
            } else if(dirvToSpringTarget.length() < 4) {
                this.setPosition(myPosition.clone().add(dirvToSpringTarget), false);
                simulationGlobals.requestGraphicsUpdate = true;
                simulationGlobals.requestAnimationUpdate = true;
            } else {
                this.setPosition(myPosition.clone().add(smoothedOffset), true);
                simulationGlobals.requestGraphicsUpdate = true;
                simulationGlobals.requestAnimationUpdate = true;
            }
        } else {
            // wut?
        }
    }

    effectAnimation() {
        let activeEffects = [];
        for(let effect of this.attachedEffects) {
            if(!effect.finished()) {
                effect.updateAnimation();
                activeEffects.push(effect);
                simulationGlobals.requestAnimationUpdate = true;
                simulationGlobals.requestGraphicsUpdate = true;
            } else {
                effect.destroy();
            }
        }
        this.attachedEffects = activeEffects;
    }

    updateAnimation() {
        this.effectAnimation();

        if(this.superNodeByConfig || this.neighborDevices.length > 1) {
            this.superNodeAnimation();
        } else {
            this.edgeNodeAnimation();
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
            text.position.x -= Math.round(text.width / 2.0);
            text.position.y -= 30;
            this.graphicsObjectInfo["object"].addChild(text);
        }

        for(let effect of this.attachedEffects) {
            effect.updateGraphics();
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
        if(this.status != newStatus["state"]) {
            if(this.status !== null) {
                let color = 0xff0000;
                if(newStatus["state"] === true) color = 0x00ff00;
                let radius = simulationGlobals.viewport.screenHeight > simulationGlobals.viewport.screenWidth ? simulationGlobals.viewport.screenHeight : simulationGlobals.viewport.screenWidth;
                this.attachedEffects.push(new FocusEffect(radius, color, this.graphicsObjectInfo["object"]));
                simulationGlobals.requestAnimationUpdate = true;
                simulationGlobals.requestGraphicsUpdate = true;
            }
            this.status = newStatus["state"];
            this.graphicsDirty = true;
        }

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

    setPosition(position, skipRounding=false) {
        if(!skipRounding) {
            position = new Victor(Math.round(position.x), Math.round(position.y));
        }
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

    requestPosition(newPosition) {
        newPosition = new Victor(Math.round(newPosition.x), Math.round(newPosition.y));
        if(this.position && (newPosition.x == this.position.x && newPosition.y == this.position.y)) {
            return;
        }
        this.requestedPosition = newPosition;
        simulationGlobals.requestGraphicsUpdate = true;
        simulationGlobals.requestAnimationUpdate = true;
    }
}