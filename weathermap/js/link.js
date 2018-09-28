import {getLinkColor} from "./util.js";

export default class Link {
    constructor(remoteInterfaceName, sourceInterface) {
        this.remoteInterfaceName = remoteInterfaceName;
        this.graphicsObjectInfo = null;
        this.dirty = false;
        this.sourceInterface = sourceInterface;
        console.log("        create link -> " + remoteInterfaceName);
    }

    destroy() {
        console.log("        destroy link -> " + this.remoteInterfaceName);
        this.sourceInterface.connectedToInterface = null;
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

        let color = this.isUp() ? getLinkColor(this.getUtilization(), 1.0) : 0xff00ff;

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

    isUp() {
        if(this.sourceInterface.statisticsData === null) {
            return false;
        } else {
            return this.sourceInterface.statisticsData.status === 1;
        }
    }

    getUtilization() {
        if(this.sourceInterface.statisticsData !== null) {
            return this.sourceInterface.statisticsData["tx_mbps"]/this.sourceInterface.statisticsData["speed_mbps"];
        } else if(this.sourceInterface.connectedToInterface !== null && this.sourceInterface.connectedToInterface.statisticsData !== null) {
            return this.sourceInterface.connectedToInterface.statisticsData["rx_mbps"]/this.sourceInterface.connectedToInterface.statisticsData["speed_mbps"];
        } else {
            return 0;
        }
    }
}