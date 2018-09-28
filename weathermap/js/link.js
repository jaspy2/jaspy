import {getLinkColor} from "./util.js";

export default class Link {
    constructor(name) {
        this.name = name;
        this.graphicsObjectInfo = null;
        this.dirty = false;
        console.log("        create link -> " + name);
    }

    destroy() {
        console.log("        destroy link -> " + this.name);
        if(this.graphicsObjectInfo !== null) {
            this.graphicsObjectInfo["attachedTo"].removeChild(this.graphicsObjectInfo["object"]);
            this.graphicsObjectInfo = null;
        }
    }

    updateGraphics(viewport, startPosition, endPosition, widthOffset1, widthOffset2) {
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": viewport
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }

        let leftUpper = startPosition.clone().add(widthOffset1);
        let leftLower = startPosition.clone().add(widthOffset2);
        let rightUpper = endPosition.clone().add(widthOffset1);
        let rightLower = endPosition.clone().add(widthOffset2);

        let color = getLinkColor(this.getUtilization(), 1.0);

        this.dirty = true;
        if(this.dirty) {
            let obj = this.graphicsObjectInfo["object"];
            obj.clear();
            obj.beginFill(color);
            obj.lineStyle(0,0x000000);
            obj.moveTo(leftUpper.x, leftUpper.y);
            obj.lineTo(leftLower.x, leftLower.y); obj.lineTo(rightLower.x, rightLower.y); obj.lineTo(rightUpper.x, rightUpper.y); obj.lineTo(leftUpper.x, leftUpper.y);
        }
    }

    getUtilization() {
        return Math.random();
    }
}