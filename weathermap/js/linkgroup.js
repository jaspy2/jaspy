import Link from "./link.js";
import {getLinkColor,hostnameFromFQDN} from "./util.js";


export default class LinkGroup {
    constructor(source, name) {
    
        this.name = name;
        this.target = name;
        this.targetPosition = new Victor(0, 0);
        this.groupedLinks = {};
        this.graphicsObjectInfo = null;
        this.position = null;
        this.graphicsDirty = false;
        this.source = source;
        this.label = new PIXI.Text("<loading>", {fontFamily : '"Courier New", Courier, monospace', fontSize: 14, fill : 0xffffff, align : 'left'});

        console.log("        create linkgroup -> " + name);
    }

    destroy() {
        console.log("        destroy linkgroup -> " + this.name);
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            this.groupedLinks[key].destroy();
            delete this.groupedLinks[key];
        }

        if(this.graphicsObjectInfo !== null) {
            this.graphicsObjectInfo["attachedTo"].removeChild(this.graphicsObjectInfo["object"]);
            this.graphicsObjectInfo = null;
        }
    }

    updateTopologyData(linkgroupInfo) {
        console.log("    update linkgroup " + this.name);
        for(let [key, value] of Object.entries(linkgroupInfo)) {
            if(!(key in this.groupedLinks)) {
                this.groupedLinks[value["connectedTo"]["interface"]] = new Link(value["connectedTo"]["interface"], value["interface"]);
                this.graphicsDirty = true;
            }
        }
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            if(!(key in linkgroupInfo)) {
                this.groupedLinks[key].destroy();
                delete this.groupedLinks[key];
                this.graphicsDirty = true;
            }
        }
    }

    updateTargetInfo(targetDevice) {
        this.targetPosition = targetDevice.getPosition();
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            let remoteInterface = targetDevice.interfaces[value.remoteInterfaceName];
            value.sourceInterface.connectedToInterface = remoteInterface;
        }
    }

    bindLabel(graphicsObject) {
        graphicsObject.linkgroup = this;
        graphicsObject.interactive = true;

        graphicsObject.mousemove = function(ev) {
            if(this.showLabel) {
                let curPoint = ev.data.global;
                curPoint.x += 20;
                let worldPoint = simulationGlobals.viewport.toWorld(curPoint);
                this.linkgroup.label.position = worldPoint;
                this.linkgroup.label.text = hostnameFromFQDN(this.linkgroup.source) + " >> " + hostnameFromFQDN(this.linkgroup.target) + "\n" + this.linkgroup.totalUsage().toFixed(2) + " Mbps";
            }
        }
        graphicsObject.mouseout = function(ev) {
            this.showLabel = false;
            simulationGlobals.viewport.removeChild(this.linkgroup.label);
        };
        graphicsObject.mouseover = function(ev) {
            this.showLabel = true;
            simulationGlobals.viewport.addChild(this.linkgroup.label);
        };
    }

    interfacesUpdated() {
        this.graphicsDirty = true;
    }

    updateGraphics(viewport, localCoord, remoteCoord) {
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": viewport
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
            this.bindLabel(this.graphicsObjectInfo["object"]);
        }

        let midpoint = localCoord.clone().multiply(new Victor(-1,-1)).add(remoteCoord).multiply(new Victor(0.5,0.5));
        let dir = midpoint.clone().normalize();
        let sides = dir.clone().rotateDeg(90);
        let candidatePosition = localCoord.clone().add(midpoint);
        if(this.position != candidatePosition) {
            this.setPosition(candidatePosition);
        }

        if(this.graphicsDirty) {
            let arrowWidth = 10.0;
            let arrowLength = 20.0;
            let dirScaled = dir.clone().multiply(new Victor(arrowLength, arrowLength));
            let sidesScaled = sides.clone().multiply(new Victor(arrowWidth, arrowWidth));
            let arrowBackLeft = new Victor(0,0).subtract(dirScaled).subtract(sidesScaled);
            let arrowBackRight = new Victor(0,0).subtract(dirScaled).add(sidesScaled);
            let boxBackLeft = new Victor(0,0).subtract(sidesScaled).subtract(dirScaled.clone().multiply(new Victor(0.5,0.5)));
            let boxBackRight = new Victor(0,0).add(sidesScaled).subtract(dirScaled.clone().multiply(new Victor(0.5,0.5)));
            let startOffset = new Victor(0,0).subtract(dir.clone().multiply(new Victor(1,1)));

            let linkLineStartPosMiddle = localCoord;
            let linkLineEndposMiddle = this.position.clone().add(startOffset).subtract(dirScaled).add(dir);

            let numLinks = Object.keys(this.groupedLinks).length;
            let width = 10;
            let widthPerLink = width/numLinks;

            let linknum = 0;
            let utilTotal = 0;
            let linksUp = 0;
            for(let [key, value] of Object.entries(this.groupedLinks)) {
                let widthOffset1 = sides.clone().multiply(new Victor(width/-2.0, width/-2.0)).add(sides.clone().multiply(new Victor(linknum*widthPerLink, linknum*widthPerLink)));
                let widthOffset2 = widthOffset1.clone().add(sides.clone().multiply(new Victor(widthPerLink, widthPerLink)));
                value.updateGraphics(viewport, linkLineStartPosMiddle, linkLineEndposMiddle, widthOffset1, widthOffset2);
                utilTotal += value.getUtilization();
                if(value.isUp()) linksUp += 1;
            }

            let avgUtil = utilTotal / numLinks;
            let color = getLinkColor(avgUtil, 1.0);
            if(linksUp === 0) {
                color = 0xff00ff;
            }

            if(linksUp > 0) {
                let obj = this.graphicsObjectInfo["object"];
                obj.clear();
                obj.beginFill(color);
                obj.lineStyle(0,0x000000);
                obj.moveTo(startOffset.x,startOffset.y);
                obj.lineTo(arrowBackLeft.x, arrowBackLeft.y); obj.lineTo(arrowBackRight.x,arrowBackRight.y); obj.lineTo(startOffset.x,startOffset.y);
                obj.endFill();
                obj.position.set(this.position.x, this.position.y);
            } else {
                let obj = this.graphicsObjectInfo["object"];
                obj.clear();
                obj.beginFill(color);
                obj.lineStyle(0,0x000000);
                obj.moveTo(boxBackLeft.x, boxBackLeft.y);
                obj.lineTo(arrowBackLeft.x, arrowBackLeft.y); obj.lineTo(arrowBackRight.x,arrowBackRight.y); obj.lineTo(boxBackRight.x,boxBackRight.y); obj.lineTo(boxBackLeft.x, boxBackLeft.y);
                obj.endFill();
                obj.position.set(this.position.x, this.position.y);
            }
            this.graphicsDirty = false;
        }
    }

    totalUsage() {
        let usage = 0;
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            usage += value.getUsage();
        }
        return usage;
    }

    setPosition(position) {
        position = new Victor(Math.round(position.x), Math.round(position.y));
        if(this.position && (position.x == this.position.x && position.y == this.position.y)) {
            return;
        }
        this.position = position;
        this.graphicsDirty = true;
    }
}