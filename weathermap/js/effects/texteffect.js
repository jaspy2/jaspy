export default class TextEffect {
    constructor(offset, text, color, attachedTo, alphaRate=1) {
        this.offset = offset;
        this.text = text;
        this.color = color;
        this.attachedTo = attachedTo;
        this.graphicsDirty = true;
        this.graphicsObjectInfo = null;
        this.alpha = 255;
        this.alphaRate = alphaRate;
    }

    destroy() {
        if(this.graphicsObjectInfo) {
            this.graphicsObjectInfo["attachedTo"].removeChild(this.graphicsObjectInfo["object"]);
        }
    }

    updateAnimation() {
        if(this.alpha > 0) {
            this.alpha -= this.alphaRate;
        }
        if(this.alpha < 0) this.alpha = 0;
        this.graphicsDirty = true;
    }

    updateGraphics() {
        if(this.graphicsObjectInfo == null) {
            let textObject = new PIXI.Text(this.text, {fontFamily : '"Courier New", Courier, monospace', fontSize: 14, fill : this.color, align : 'left'});
            textObject.interactive = false;
            textObject.buttonMode = false;
            textObject.position.x = this.offset.x;
            textObject.position.y = this.offset.y + textObject.height/2.0;
            this.graphicsObjectInfo = {
                "object": textObject,
                "attachedTo": this.attachedTo
            };
            this.graphicsObjectInfo["attachedTo"].addChild(this.graphicsObjectInfo["object"]);
        }
        this.graphicsObjectInfo["object"].alpha = this.alpha/255.0;
        this.graphicsDirty = false;
    }

    finished() {
        if(this.alpha <= 0) {
            return true;
        } else {
            return false;
        }
    }
}