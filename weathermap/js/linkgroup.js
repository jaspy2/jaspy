import Link from "./link.js";


export default class LinkGroup {
    constructor(source, name) {
        this.name = name;
        this.target = name;
        this.targetPosition = new Victor(0, 0);
        this.groupedLinks = {};
        this.graphicsObjectInfo = null;
        this.position = new Victor(0,0);
        console.log("        create linkgroup -> " + name);
    }

    destroy() {
        console.log("        destroy linkgroup -> " + this.name);
        for(let [key, value] of Object.entries(this.groupedLinks)) {
            this.groupedLinks[key].destroy();
            delete this.groupedLinks[key];
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
        let midpoint = localCoord.clone().multiply(new Victor(-1,-1)).add(remoteCoord).multiply(new Victor(0.5,0.5));
        let dir = midpoint.clone().normalize();
        let sides = dir.clone().rotateDeg(90);
        this.position = localCoord.clone().add(midpoint);

        let arrowWidth = 10.0;
        let arrowLength = 20.0;
        let dirScaled = dir.clone().multiply(new Victor(arrowLength, arrowLength));
        let sidesScaled = sides.clone().multiply(new Victor(arrowWidth, arrowWidth));
        let arrowBackLeft = new Victor(0,0).subtract(dirScaled).subtract(sidesScaled);
        let arrowBackRight = new Victor(0,0).subtract(dirScaled).add(sidesScaled);
        let startOffset = new Victor(0,0).subtract(dir.clone().multiply(new Victor(1,1)));

        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": viewport
            };
            let obj = this.graphicsObjectInfo["object"];
            obj.clear();
            obj.beginFill(0xff0000);
            obj.lineStyle(0,0x00ffff);
            obj.moveTo(startOffset.x,startOffset.y);
            obj.lineTo(arrowBackLeft.x, arrowBackLeft.y); obj.lineTo(arrowBackRight.x,arrowBackRight.y); obj.lineTo(startOffset.x,startOffset.y);
            obj.endFill();
            obj.position.set(this.position.x, this.position.y);
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }
    }
}