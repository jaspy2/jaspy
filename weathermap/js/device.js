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
        this.superNode = null;
        this.superNodeByConfig = false;
        this.expanded = null;
        this.expandedByDefault = false;

        //console.log("create device " + fqdn)
    }

    isExpanded() {
        if(this.expanded === null) {
            return this.expandedByDefault;
        } else {
            return this.expanded;
        }
    }

    armDragEvents(object) {
        object.device = this;
        object.interactive = true;
        object.buttonMode = true;

        object.mousedown = object.touchstart = function(e) {
            if(e.data.originalEvent.shiftKey) {
                this.device.dragInfo = {};
                simulationGlobals.requestGraphicsUpdate = true;
                simulationGlobals.requestAnimationUpdate = true;
                simulationGlobals.positionUpdatesFrozen = true;
                e.stopPropagation();
            } else {
                this.device.expanded = !this.device.isExpanded();
                simulationGlobals.requestGraphicsUpdate = true;
                simulationGlobals.requestAnimationUpdate = true;
                e.stopPropagation();
            }
        };

        object.mouseup = object.mouseupoutside = object.touchend = object.touchendoutside = function(e) {
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

        object.mousemove = object.touchmove = function(e) {
            if(this.device.dragInfo !== null) {
                let curPoint = e.data.global;
                let worldPoint = simulationGlobals.viewport.toWorld(curPoint);
                this.device.setPosition(new Victor(worldPoint.x, worldPoint.y));
                simulationGlobals.requestGraphicsUpdate = true;
                simulationGlobals.requestAnimationUpdate = true;
                e.stopPropagation();
            }
        };
    }

    destroy() {
        //console.log("destroy device " + this.fqdn)
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
        //console.log("update device " + this.fqdn)
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
        if(this.neighborDevices.length > 1 || this.neighborDevices.length == 0) {
            this.superNode = true;
        } else {
            this.superNode = false;
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
                this.setPosition(this.position.clone().add(factoredOffset));
            }
            simulationGlobals.requestGraphicsUpdate = true;
            simulationGlobals.requestAnimationUpdate = true;
        }
    }

    edgeNodeAnimation() {
        if(this.neighborDevices.length != 1) return;
        if(this.dragInfo) return;

        let myPosition = this.getPosition().clone();
        let offsetVector = new Victor(0,0);

        let nbr = this.neighborDevices[0];
        
        let springDistance = config.springDistance;
        if(this.isLargeIcon()) {
            springDistance *= 3.0;
        }
        
        if(true) {
            

            let nbrPosition = nbr.getPosition();

            let dirvToNbr = nbrPosition.clone().subtract(myPosition).normalize();
            let springTarget = nbrPosition.clone().subtract(dirvToNbr.clone().multiply(new Victor(springDistance, springDistance)));
            let dirvToSpringTarget = springTarget.subtract(myPosition);
            let smoothedOffset = dirvToSpringTarget.clone().multiply(new Victor(0.2,0.2));

            if(dirvToSpringTarget.length() < 1) {
                offsetVector.add(dirvToSpringTarget);
            } else {
                offsetVector.add(smoothedOffset);
            }
        }

        if(true) {
            let clusterCenter = this.neighborDevices[0];
            let selfDV = this.getPosition().clone().subtract(clusterCenter.getPosition()).normalize();
            let selfPerp = selfDV.clone().rotateDeg(90);
            let forceOffset = new Victor(0,0);
            for(let nbrNbr of clusterCenter.neighborDevices) {
                if(nbrNbr == this) continue;
                let nbrDV = nbrNbr.getPosition().clone().subtract(clusterCenter.getPosition()).normalize();
                let invNbrDV = nbrDV.clone().invert();
                let angle = Math.acos(selfDV.clone().dot(invNbrDV))*Math.sign(invNbrDV.clone().cross(selfDV));
                let angleFactor = -angle * (springDistance / 20.0);
                forceOffset.add(selfPerp.clone().multiply(new Victor(angleFactor, angleFactor)));
            }
            offsetVector.add(forceOffset);
        }

        if(offsetVector.length() > 0.1) {
            simulationGlobals.requestGraphicsUpdate = true;
            simulationGlobals.requestAnimationUpdate = true;
        }
        this.setPosition(myPosition.clone().add(offsetVector));
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

        if(this.isSuperNode()) {
            this.superNodeAnimation();
        } else {
            this.edgeNodeAnimation();
        }
    }

    isLargeIcon() {
        let largeIcon = this.isExpanded() || this.isSuperNode();
        if(!this.isSuperNode()) {
            if(this.neighborDevices[0].isExpanded()) largeIcon = true;
        }
        return largeIcon;
    }

    isSuperNode() {
        if(this.superNodeByConfig) return true;
        return this.superNode;
    }

    updateGraphics(linkLayer, deviceLayer, devices) {
        let largeIcon = this.isLargeIcon();
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": deviceLayer
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
            this.armDragEvents(this.graphicsObjectInfo["object"]);

            this.text = new PIXI.Text(this.hostname, {fontFamily : '"Courier New", Courier, monospace', fontSize: 14, fill : 0xffffff, align : 'center'});
            this.text.position.x -= Math.round(this.text.width / 2.0);
            this.text.position.y -= (config.deviceIconSize + 10);
            this.graphicsObjectInfo["object"].addChild(this.text);
        }

        if(!largeIcon || !this.isSuperNode()) {
            let dvFromPeer = this.getPosition().clone().subtract(this.neighborDevices[0].getPosition()).normalize();
            let perpDVFromPeer = dvFromPeer.clone().rotateDeg(90).normalize();
            let horAngle = dvFromPeer.horizontalAngle();
            let offset = config.deviceIconSize;
            if(!largeIcon) {
                offset /= 4.0;
            }
            if(Math.abs(horAngle) > (Math.PI/2.0)) {
                horAngle += Math.PI;
                this.text.position.x = dvFromPeer.x * (offset + this.text.width) + perpDVFromPeer.x * this.text.height / 2.0;
                this.text.position.y = dvFromPeer.y * (offset + this.text.width) + perpDVFromPeer.y * this.text.height / 2.0;
            } else {
                this.text.position.x = dvFromPeer.x * (offset) - perpDVFromPeer.x * this.text.height / 2.0;
                this.text.position.y = dvFromPeer.y * (offset) - perpDVFromPeer.y * this.text.height / 2.0;
            }
            this.text.rotation = horAngle;
        } else {
            this.text.rotation = 0;
            this.text.position.x = -Math.round(this.text.width / 2.0);
            this.text.position.y = -(config.deviceIconSize + 10);
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
            let radius = config.deviceIconSize / 2.0;
            let lineWidth = 2;
            if(!largeIcon) {
                radius = radius / 4.0;
                lineWidth = 0;
            }
            obj.beginFill(fillcolor);
            obj.lineStyle(lineWidth,0x777777);
            obj.moveTo(-radius,-radius);
            obj.lineTo(-radius,radius); obj.lineTo(radius,radius); obj.lineTo(radius,-radius); obj.lineTo(-radius,-radius);
            obj.endFill();
            obj.position.set(this.position.x, this.position.y);
            this.graphicsDirty = false;
        }

        for(let [key, value] of Object.entries(this.linkGroups)) {
            let localCoord = this.getPosition();
            let remoteCoord = devices[key].getPosition().clone();
            let largeLink = largeIcon && devices[key].isLargeIcon();
            value.updateGraphics(linkLayer, localCoord, remoteCoord, largeLink);
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

    setPosition(position) {
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

    updateWeathermapPosition(data) {
        let changed = false;
        
        this.requestPosition(new Victor(data['x'], data['y']));
        
        if(this.expandedByDefault != data.expandedByDefault) {
            changed = true;
            this.expandedByDefault = data.expandedByDefault;
        }
        
        if(this.superNodeByConfig != data.superNode) {
            changed = true;
            this.superNodeByConfig = data.superNode;
        }

        if(changed) {
            simulationGlobals.requestGraphicsUpdate = true;
            simulationGlobals.requestAnimationUpdate = true;
        }
    }
}
