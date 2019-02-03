export default class FocusEffect {
    constructor(radius, color, attachedTo) {
        this.radius = radius;
        this.color = color;
        this.attachedTo = attachedTo;
        this.graphicsDirty = true;
        this.graphicsObjectInfo = null;
    }

    destroy() {
        if(this.graphicsObjectInfo) {
            this.graphicsObjectInfo["attachedTo"].removeChild(this.graphicsObjectInfo["object"]);
        }
    }

    updateAnimation() {
        this.radius *= 0.95;
        if(this.radius < 1) this.radius = 0;
        this.graphicsDirty = true;
    }

    updateGraphics() {
        if(this.graphicsObjectInfo == null) {
            this.graphicsObjectInfo = {
                "object": new PIXI.Graphics(),
                "attachedTo": this.attachedTo
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }

        let graphicsObject = this.graphicsObjectInfo["object"];
        graphicsObject.clear();
        graphicsObject.beginFill(this.color, 0.25);
        graphicsObject.drawCircle(0, 0, this.radius);

        this.graphicsDirty = false;
    }

    finished() {
        if(this.radius <= 0) {
            return true;
        } else {
            return false;
        }
    }
}