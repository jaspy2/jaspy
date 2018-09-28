

export default class Link {
    constructor(name) {
        this.name = name;
        this.graphicsObjectInfo = null;
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
        let leftUpper = startPosition.clone().add(widthOffset1);
        let leftLower = startPosition.clone().add(widthOffset2);
        let rightUpper = endPosition.clone().add(widthOffset1);
        let rightLower = endPosition.clone().add(widthOffset2);

        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": viewport
            };
            let obj = this.graphicsObjectInfo["object"];
            obj.clear();
            obj.beginFill(0xff00ff);
            obj.lineStyle(0,0x00ffff);
            obj.moveTo(leftUpper.x, leftUpper.y);
            obj.lineTo(leftLower.x, leftLower.y); obj.lineTo(rightLower.x, rightLower.y); obj.lineTo(rightUpper.x, rightUpper.y); obj.lineTo(leftUpper.x, leftUpper.y);
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }
    }
}