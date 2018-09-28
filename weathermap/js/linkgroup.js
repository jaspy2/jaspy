import Link from "./link.js";
import {getLinkColor} from "./util.js";


export default class LinkGroup {
    constructor(source, name) {
    
        this.name = name;
        this.target = name;
        this.targetPosition = new Victor(0, 0);
        this.groupedLinks = {};
        this.graphicsObjectInfo = null;
        this.setPosition(null);
        this.dirty = false;
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
                this.groupedLinks[value["connectedTo"]["interface"]] = new Link(value["connectedTo"]["interface"]);
            }
        }
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            if(!(key in linkgroupInfo)) {
                this.groupedLinks[key].destroy();
                delete this.groupedLinks[key];
            }
        }
    }

    updateTargetInfo(targetDevice) {
        this.targetPosition = targetDevice.getPosition();
    }

    updateGraphics(viewport, localCoord, remoteCoord) {
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": viewport
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }

        let midpoint = localCoord.clone().multiply(new Victor(-1,-1)).add(remoteCoord).multiply(new Victor(0.5,0.5));
        let dir = midpoint.clone().normalize();
        let sides = dir.clone().rotateDeg(90);
        let candidatePosition = localCoord.clone().add(midpoint);
        if(this.position != candidatePosition) {
            this.setPosition(candidatePosition);
        }

        this.dirty = true;
        if(this.dirty) {
            let arrowWidth = 10.0;
            let arrowLength = 20.0;
            let dirScaled = dir.clone().multiply(new Victor(arrowLength, arrowLength));
            let sidesScaled = sides.clone().multiply(new Victor(arrowWidth, arrowWidth));
            let arrowBackLeft = new Victor(0,0).subtract(dirScaled).subtract(sidesScaled);
            let arrowBackRight = new Victor(0,0).subtract(dirScaled).add(sidesScaled);
            let startOffset = new Victor(0,0).subtract(dir.clone().multiply(new Victor(1,1)));

            let linkLineStartPosMiddle = localCoord;
            let linkLineEndposMiddle = this.position.clone().add(startOffset).subtract(dirScaled).add(dir);

            let numLinks = Object.keys(this.groupedLinks).length;
            let width = 10;
            let widthPerLink = width/numLinks;

            let linknum = 0;
            let utilTotal = 0;
            for(let [key, value] of Object.entries(this.groupedLinks)) {
                let widthOffset1 = sides.clone().multiply(new Victor(width/-2.0, width/-2.0)).add(sides.clone().multiply(new Victor(linknum*widthPerLink, linknum*widthPerLink)));
                let widthOffset2 = widthOffset1.clone().add(sides.clone().multiply(new Victor(widthPerLink, widthPerLink)));
                value.updateGraphics(viewport, linkLineStartPosMiddle, linkLineEndposMiddle, widthOffset1, widthOffset2);
                utilTotal += value.getUtilization();
            }

            let avgUtil = utilTotal / numLinks;
            let color = getLinkColor(avgUtil, 1.0);

            let obj = this.graphicsObjectInfo["object"];
            obj.clear();
            obj.beginFill(color);
            obj.lineStyle(0,0x000000);
            obj.moveTo(startOffset.x,startOffset.y);
            obj.lineTo(arrowBackLeft.x, arrowBackLeft.y); obj.lineTo(arrowBackRight.x,arrowBackRight.y); obj.lineTo(startOffset.x,startOffset.y);
            obj.endFill();
            obj.position.set(this.position.x, this.position.y);
        }
    }

    setPosition(position) {
        this.position = position;
        this.dirty = true;
    }
}